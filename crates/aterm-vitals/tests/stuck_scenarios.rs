// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scenario tests for the stuck-process engine.

#[path = "support/world.rs"]
mod world;

#[path = "support/scenarios.rs"]
mod scenarios;

use aterm_vitals::jobs::wire::{exe_word, is_exe_word};
use aterm_vitals::jobs::{
    Block, Class, Consumer, Demand, Findings, GateWord, HeldEpisode, JobKind, JobsView, MuFrom,
    Named, NamedWait, OWN_MIN, ProcState, TermWord, TierB, Verdict, WaitName, command_class,
};
use scenarios::{TICK_MS, curl, follow_fds, pipe_end, sleeper, socket_fd, spinner, tcp_fd};
use world::{OutFile, Proc, World, hint};

fn run_to(w: &mut World, until_ms: u64, mut each: impl FnMut(&mut World, u64)) -> JobsView {
    let mut rel = 0;
    each(w, 0);
    let mut view = w.observe();
    while rel < until_ms {
        let step = TICK_MS.min(until_ms - rel);
        rel += step;
        w.advance(step);
        each(w, rel);
        view = w.observe();
    }
    view
}

/// The person types a line into the session `reader` reads, through its relay in front of tab 1,
/// a tick after the session sat still.
fn typed_into(w: &mut World, reader: u32) {
    let _ = run_to(w, 4 * TICK_MS, |w, rel| {
        if rel == 2 * TICK_MS || rel == 4 * TICK_MS {
            w.typed(1, reader);
        }
    });
}

fn wire(v: &JobsView, id: &str) -> String {
    v.row(id)
        .unwrap_or_else(|| {
            panic!(
                "no row {id}: {:?}",
                v.rows.iter().map(|r| r.wire()).collect::<Vec<_>>()
            )
        })
        .wire()
}

#[test]
fn example_a_password_prompt_in_a_background_tab_waits_for_you() {
    let mut w = World::new();
    w.shell_tab(3, 3, 230);
    w.run_fg(3, 51, "ssh host", sleeper(2300, "ssh"));
    w.tty_mode(3, true, false);
    let v = run_to(&mut w, 252_000, |_, _| {});
    assert_eq!(
        wire(&v, "b51"),
        "job id=b51 kind=shell consumer=person-fg exe=ssh class=waiting wait=tty-password quiet_ms=252000 phi=0 verdict=waiting-you"
    );
    assert_eq!(v.demand, Demand::Idle);
}

#[test]
fn example_b_an_agent_call_hung_on_a_silent_tcp_peer_is_stuck() {
    let mut w = World::new();
    w.call(
        1,
        4000,
        4711,
        curl(4712),
        Some(hint(4711, Consumer::AgentFg, Some("bm3k2x9q1"), None)),
    );
    let v = run_to(&mut w, 1_142_000, |w, rel| {
        if rel == 120_000 {
            w.hint(
                1,
                hint(4711, Consumer::AgentBg, Some("bm3k2x9q1"), Some(120_000)),
            );
        }
    });
    assert_eq!(
        wire(&v, "a4711"),
        "job id=a4711 kind=agent-call consumer=agent-bg promoted=timeout:120000 task=bm3k2x9q1 pgid=4711 exe=curl class=waiting wait=tcp remote=192.0.2.10:8080 state=ESTABLISHED queued=0 quiet_ms=1142000 phi=8.27 mu_ms=60000 mu_from=prior verdict=stuck"
    );
    let row = v.row("a4711").expect("the call");
    assert_eq!(row.verdict.mark(), Some('⊘'));
    assert_eq!(v.demand, Demand::Fast);
}

#[test]
fn example_c_a_cargo_lock_wait_is_capped_until_named_then_fine_behind_a_running_holder() {
    let mut w = World::new();
    for _ in 0..3 {
        w.jobs.learn(
            &command_class("cargo build --release", aterm_vitals::jobs::JobKind::Shell),
            12_000,
        );
    }
    w.shell_tab(2, 2, 210);
    let mut holder = sleeper(5120, "cargo");
    holder.child_cpu_per_s = 1_800_000_000;
    w.run_fg(2, 61, "cargo build", holder);
    w.run_fg(1, 77, "cargo build --release", sleeper(7700, "cargo"));
    let churn = |w: &mut World, rel: u64| scenarios::churn(w, 5120, 5120, rel, "rustc");
    let v = run_to(&mut w, 420_000, churn);
    assert_eq!(
        wire(&v, "b77"),
        "job id=b77 kind=shell consumer=person-fg exe=cargo class=waiting wait=unnamed quiet_ms=420000 phi=15.2 mu_ms=12000 mu_from=class verdict=quiet"
    );
    assert_eq!(v.row("b61").map(|r| r.class), Some(Class::Running));
    w.advance(TICK_MS);
    churn(&mut w, 422_000);
    w.name(
        1,
        NamedWait {
            leader: 7700,
            word: TierB::Lock,
            holder: Some(5120),
        },
    );
    let held = w.observe();
    assert_eq!(held.row("b77").map(|r| r.verdict), Some(Verdict::Quiet));
    w.advance(TICK_MS);
    churn(&mut w, 424_000);
    let v = w.observe();
    assert_eq!(
        wire(&v, "b77"),
        "job id=b77 kind=shell consumer=person-fg exe=cargo class=waiting wait=unnamed named=lock holder=5120:cargo:tab2:running quiet_ms=424000 phi=0 verdict=fine"
    );
}

#[test]
fn example_d_tail_f_in_an_agent_foreground_call_never_returns() {
    let mut w = World::new();
    let mut tail = sleeper(8103, "tail");
    tail.fds = follow_fds();
    w.call(
        1,
        8000,
        8102,
        tail,
        Some(hint(8102, Consumer::AgentFg, Some("c81d0f2ma"), None)),
    );
    let v = run_to(&mut w, 41_000, |_, _| {});
    assert_eq!(
        wire(&v, "a8102"),
        "job id=a8102 kind=agent-call consumer=agent-fg task=c81d0f2ma pgid=8102 exe=tail class=waiting wait=follow fd=3 quiet_ms=41000 phi=0 verdict=never-returns"
    );
}

#[test]
fn example_e_a_spinning_full_screen_program_with_an_unread_key_is_the_gates_stuck() {
    let mut w = World::new();
    let mut dash = spinner(9300, "dash");
    dash.cpu_per_s = 910_000_000;
    w.run_fg(1, 93, "./dash", dash);
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 30_000, |w, rel| {
        if rel >= 2_000 {
            let word = if rel >= 20_000 {
                GateWord::Stalled
            } else {
                GateWord::Pending
            };
            w.gate(1, word, 1, rel - 2_000);
        }
    });
    assert_eq!(
        wire(&v, "b93"),
        "job id=b93 kind=shell consumer=person-fg exe=dash class=unread-input input=stalled input_bytes=1 input_wait_ms=28000 cpu_mc=910 verdict=stuck"
    );
}

#[test]
fn example_f_a_held_stop_under_an_agent_call_is_stuck_and_a_parked_one_is_parked() {
    let mut w = World::new();
    let mut node = sleeper(6302, "node");
    node.state = ProcState::Stopped;
    w.call(
        1,
        6000,
        6301,
        node,
        Some(hint(6301, Consumer::AgentBg, Some("a71pq0zz3"), None)),
    );
    let v = run_to(&mut w, 1_130_000, |_, _| {});
    assert_eq!(
        wire(&v, "a6301"),
        "job id=a6301 kind=agent-call consumer=agent-bg task=a71pq0zz3 pgid=6301 exe=zsh class=stopped stop=held stopped_pid=6302:node quiet_ms=1130000 phi=8.18 verdict=stuck"
    );

    let mut w = World::new();
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let v = run_to(&mut w, 7_200_000, |w, rel| {
        if rel == 2_000 {
            w.p(1200).state = ProcState::Stopped;
            w.stop_block(1);
        }
    });
    let row = v.row("b12").expect("the parked job keeps its id");
    assert!(
        row.wire()
            .contains("class=stopped stop=parked stopped_pid=1200:vim"),
        "{}",
        row.wire()
    );
    assert_eq!(row.verdict, Verdict::Parked);
    assert_eq!(row.kind, aterm_vitals::jobs::JobKind::ShellBg);
    assert_eq!(v.demand, Demand::Idle, "a parked job arms nothing");
}

#[test]
fn example_g_a_zombie_pile_under_a_logging_server_is_running_with_its_zombies_counted() {
    let mut w = World::new();
    let mut node = sleeper(6120, "node");
    node.cpu_per_s = 10_000_000;
    w.run_bg(1, node);
    let v = run_to(&mut w, 2_400_000, |w, rel| {
        w.line(1);
        let k = u32::try_from(rel / 65_000).expect("small");
        if k < 37 && !w.procs.contains_key(&(6200 + k)) {
            let mut z = Proc::new(6200 + k, 6120, "node");
            z.pgid = 6120;
            z.sid = 200;
            z.tty = true;
            z.state = ProcState::Zombie;
            w.add(z);
        }
    });
    assert_eq!(
        wire(&v, "j6120"),
        "job id=j6120 kind=shell-bg consumer=person-bg exe=node class=running zombies=37 zombie_parent=6120 zombies_per_min=0.9 verdict=fine"
    );
    let lines = v.findings.wire_lines();
    assert!(
        lines
            .contains(&"finding kind=zombies parent=6120 exe=node count=37 per_min=0.9".to_owned()),
        "{lines:?}"
    );
}

#[test]
fn every_scenario_of_the_expectation_table_reads_as_the_table_says() {
    let all = scenarios::all();
    let rows: std::collections::BTreeSet<&str> = all
        .iter()
        .map(|s| s.row.split(' ').next().unwrap_or(""))
        .collect();
    for want in [
        "1", "2", "3", "3b", "4", "5", "6", "4-6", "7", "8", "9", "10", "11", "11b", "12", "13",
        "13b", "14", "15", "16", "17", "18", "19", "20", "21", "22", "23", "24", "+",
    ] {
        assert!(rows.contains(want), "row {want} has no fixture");
    }
    let mut bad = Vec::new();
    for s in &all {
        let t = scenarios::run(s, None);
        bad.extend(scenarios::check(s, &t));
    }
    assert!(
        bad.is_empty(),
        "{} expectation(s) failed:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

#[test]
fn a_call_whose_leader_exits_leaves_an_orphan_with_findings_only() {
    let mut w = World::new();
    let mut child = spinner(401, "cargo-build-script");
    child.cpu_per_s = 1_000_000_000;
    w.call(1, 300, 400, child, None);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(400, false);
    w.p(401).ppid = 1;
    let v = w.tick(TICK_MS);
    assert_eq!(v.row("a400").map(|r| r.verdict), Some(Verdict::Exited));
    let v = run_to(&mut w, 660_000, |_, _| {});
    let o = v.row("o401").expect("the orphan");
    assert_eq!(o.consumer, Consumer::Nobody);
    assert_eq!(o.verdict, Verdict::Fine);
    assert_eq!(o.phi_milli, None, "nobody awaits an orphan: no φ");
    assert!(
        v.row("a400").is_none(),
        "an exited row is kept 60 s, then gone"
    );
    assert_eq!(
        v.findings.busy_orphans.len(),
        1,
        "a core for 10 min with no side writes"
    );
    assert_eq!(v.findings.busy_orphans[0].pid, 401);
}

#[test]
fn a_call_whose_leader_exits_unreaped_reads_exited_as_the_call() {
    let mut w = World::new();
    w.call(1, 300, 400, sleeper(401, "sleep"), None);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(401, false);
    w.exit(400, true);
    let v = w.tick(TICK_MS);
    let row = v.row("a400").expect("the exited call");
    assert_eq!(
        (row.kind, row.consumer, row.verdict),
        (JobKind::AgentCall, Consumer::Agent, Verdict::Exited),
        "{}",
        row.wire()
    );
    assert!(v.row("j400").is_none(), "no shell job is made of it");
}

#[test]
fn an_exited_block_keeps_its_row_and_status_for_a_minute() {
    let mut w = World::new();
    w.run_fg(1, 7, "make", sleeper(1000, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.close_block(1, 2);
    let v = w.tick(TICK_MS);
    let row = v.row("b7").expect("the exited row");
    assert_eq!(row.verdict, Verdict::Exited);
    assert_eq!(row.exit, Some(2));
    assert!(
        row.wire().ends_with("exit=2 verdict=exited"),
        "{}",
        row.wire()
    );
    assert!(row.wire().contains(" exe=make "), "{}", row.wire());
    let v = run_to(&mut w, 58_000, |_, _| {});
    assert!(v.row("b7").is_some());
    let v = w.tick(TICK_MS);
    assert!(v.row("b7").is_none());
}

/// A COMMAND FOLLOWED BY ANOTHER BETWEEN TWO TICKS KEEPS ITS STATUS: the
/// tab's block is the next command's by the tick that would have read `D`,
/// and the ended block it replaced carries the status.
#[test]
fn a_command_followed_by_another_between_two_ticks_keeps_its_status() {
    let mut w = World::new();
    w.run_fg(1, 7, "make", sleeper(1000, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.close_block(1, 2);
    w.run_fg(1, 8, "less", sleeper(1001, "less"));
    let v = w.tick(TICK_MS);
    let row = v.row("b7").expect("the exited row");
    assert_eq!((row.verdict, row.exit), (Verdict::Exited, Some(2)));
    assert!(v.row("b8").is_some(), "the next command is its own job");
}

/// AN EXITED COMMAND'S ROW NAMES ITS PROGRAM, NEVER THE SHELL LEFT BEHIND:
/// the program exits before the shell writes `D`, and a tick between the two
/// sees the shell alone in the block.
#[test]
fn an_exited_commands_row_names_its_program_not_the_shell_left_behind() {
    let mut w = World::new();
    w.run_fg(1, 7, "top", sleeper(1000, "top"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    let _ = w.tick(TICK_MS);
    w.close_block(1, 0);
    let v = w.tick(TICK_MS);
    let row = v.row("b7").expect("the exited row");
    assert_eq!(row.verdict, Verdict::Exited);
    assert!(row.wire().contains(" exe=top "), "{}", row.wire());
}

#[test]
fn ctrl_z_carries_the_jobs_id_and_quiet_clock_into_the_background() {
    let mut w = World::new();
    w.run_fg(1, 51, "./spin", sleeper(1000, "spin"));
    let before = run_to(&mut w, 100_000, |_, _| {});
    let q = before.row("b51").and_then(|r| r.quiet_ms).expect("quiet");
    w.p(1000).state = ProcState::Stopped;
    w.stop_block(1);
    let v = w.tick(TICK_MS);
    let row = v.row("b51").expect("same id");
    assert_eq!(row.kind, aterm_vitals::jobs::JobKind::ShellBg);
    assert_eq!(row.consumer, Consumer::PersonBg);
    assert!(row.quiet_ms.is_some_and(|n| n >= q), "the clock is carried");
}

#[test]
fn demand_is_fast_for_suspects_slow_for_legit_waits_and_idle_for_the_person() {
    let mut w = World::new();
    assert_eq!(w.tick(TICK_MS).demand, Demand::Idle, "no job");
    let mut tail = sleeper(401, "tail");
    tail.fds = follow_fds();
    w.call(
        1,
        300,
        400,
        tail,
        Some(hint(400, Consumer::AgentBg, None, None)),
    );
    w.p(300).cpu_per_s = 0;
    let v = w.tick(TICK_MS);
    assert_eq!(v.demand, Demand::Fast);
    let mut w = World::new();
    let mut tail = sleeper(1000, "tail");
    tail.fds = follow_fds();
    w.run_bg(1, tail);
    assert_eq!(w.tick(TICK_MS).demand, Demand::Slow);
}

#[test]
fn findings_count_stopped_d_state_and_the_process_table() {
    let mut w = World::new();
    let mut s = sleeper(7000, "stopped-elsewhere");
    s.ppid = 1;
    s.uid = 0;
    s.state = ProcState::Stopped;
    w.add(s);
    let mut d = sleeper(7001, "fsck");
    d.ppid = 1;
    d.d_state = true;
    w.add(d);
    let v = run_to(&mut w, 12_000, |_, _| {});
    assert_eq!(
        v.findings.stopped.iter().map(|n| n.pid).collect::<Vec<_>>(),
        vec![7000]
    );
    assert_eq!(
        v.findings.d_state.iter().map(|n| n.pid).collect::<Vec<_>>(),
        vec![7001]
    );
    assert_eq!(v.findings.own_procs, 3);
    assert_eq!(v.findings.process_limit, Some(5333));
    let lines = v.findings.wire_lines();
    assert!(
        lines.contains(&"finding kind=process-table own=3 limit=5333".to_owned()),
        "{lines:?}"
    );
}

/// A held process outside every tab's tree: launchd adopted it (I1's `gh`).
fn held_orphan(pid: u32, name: &str, aterm: bool) -> Proc {
    let mut p = sleeper(pid, name);
    p.ppid = 1;
    p.held = true;
    p.aterm = aterm;
    p
}

/// What the view's program-check findings say: `(pid, for_ms, tab)` each.
fn held_rows(v: &JobsView) -> Vec<(u32, u64, Option<u64>)> {
    v.findings
        .program_check
        .iter()
        .map(|n| (n.pid, n.for_ms, n.tab))
        .collect()
}

#[test]
fn held_rows_count_from_their_first_sighting_and_open_one_episode_when_backed_up() {
    let mut w = World::new();
    let mut gh = sleeper(401, "gh");
    gh.held = true;
    w.call(1, 300, 400, gh, None);
    let v = w.observe();
    assert_eq!(held_rows(&v), vec![(401, 0, Some(1))], "a first sighting");
    assert_eq!(
        v.findings.program_check_episode, None,
        "one row at one reading: no episode yet (a normal exec reads held for under 0.3 ms)"
    );
    let opened = w.at + TICK_MS;
    let v = w.tick(TICK_MS);
    assert_eq!(held_rows(&v), vec![(401, TICK_MS, Some(1))]);
    let backed_up = HeldEpisode {
        opened_ms: opened,
        for_ms: 0,
        peak: 1,
        oldest_ms: TICK_MS,
    };
    assert_eq!(
        v.findings.program_check_episode,
        Some(backed_up),
        "held across two readings: backed up"
    );
    w.p(401).exec = 2;
    let v = w.tick(TICK_MS);
    assert_eq!(
        held_rows(&v),
        vec![(401, 0, Some(1))],
        "an exec starts a new held time"
    );
    w.p(401).held = false;
    let v = w.tick(TICK_MS);
    assert!(v.findings.program_check.is_empty());
    assert!(
        v.findings.program_check_episode.is_some() && v.findings.program_check_drained.is_none(),
        "one reading with nothing held does not close it"
    );
    let v = w.tick(TICK_MS);
    assert_eq!(v.findings.program_check_episode, None);
    assert_eq!(
        v.findings.program_check_drained,
        Some(HeldEpisode {
            for_ms: TICK_MS,
            ..backed_up
        }),
        "the second empty reading closes it: open to its last held reading"
    );
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.findings.program_check_drained, None,
        "said on one view only"
    );

    let mut w = World::new();
    w.add(held_orphan(7002, "python3", false));
    let v = w.observe();
    assert_eq!(
        v.findings.program_check_aterm,
        (0, 0),
        "started outside aterm"
    );
    let mut gh = sleeper(401, "gh");
    gh.held = true;
    w.call(1, 300, 400, gh, None);
    w.add(held_orphan(7001, "agy", true));
    let v = w.tick(TICK_MS);
    assert_eq!(
        held_rows(&v),
        vec![(401, 0, Some(1)), (7001, 0, None), (7002, TICK_MS, None)],
        "a member has its tab, an orphan none"
    );
    assert_eq!(
        v.findings.program_check_aterm,
        (2, 0),
        "aterm's: the member and the one aterm's coalition holds, and their oldest"
    );

    let mut w = World::new();
    for (pid, name) in [(7001, "agy"), (7003, "otool"), (7004, "codex")] {
        w.add(held_orphan(pid, name, true));
    }
    let at = w.at;
    let v = w.observe();
    assert_eq!(
        v.findings.program_check_episode,
        Some(HeldEpisode {
            opened_ms: at,
            for_ms: 0,
            peak: 3,
            oldest_ms: 0,
        }),
        "three at one reading: a queue"
    );
}

#[test]
fn a_held_program_leaves_demand_to_the_jobs() {
    let mut w = World::new();
    w.add(held_orphan(7001, "gh", true));
    let v = w.observe();
    assert!(v.rows.is_empty(), "no job: {:?}", v.rows);
    assert_eq!(
        v.demand,
        Demand::Idle,
        "no job to watch: a window's bars keep the idle engine's cadence (the host reads a held \
         program with no window)"
    );
    let v = w.tick(TICK_MS);
    assert!(v.findings.program_check_episode.is_some());
    assert_eq!(v.demand, Demand::Idle, "an open episode is no job either");
}

#[test]
fn a_held_finding_prints_one_row_with_its_age() {
    let f = Findings {
        program_check: vec![
            Named {
                pid: 25311,
                name: "gh".into(),
                tab: None,
                for_ms: 35_940_000,
            },
            Named {
                pid: 72969,
                name: "reroute codex\u{1b}[31m".into(),
                tab: Some(3),
                for_ms: 1_140_000,
            },
        ],
        own_procs: 576,
        ..Findings::default()
    };
    assert_eq!(
        f.wire_lines(),
        [
            "finding kind=program-check pid=25311 exe=gh for_ms=35940000",
            "finding kind=program-check pid=72969 exe=- for_ms=1140000",
            "finding kind=process-table own=576",
        ]
    );
}

/// A ROW THAT HAS NOT STARTED FOR ANOTHER REASON IS NAMED BY IT, AND IS NO PART OF THE CHECK'S
/// EPISODE: an x86_64 orphan Rosetta translates and one on a volume not proven local are each
/// counted from their first sighting under their own kind (`translating`, `not-started`), and
/// three of them across two readings open no backed-up episode of macOS's program check, nor
/// count as held.
#[test]
fn a_translating_or_unstarted_row_is_named_by_its_wait_and_opens_no_episode() {
    use aterm_vitals::jobs::StartWait;
    let mut w = World::new();
    for (pid, name, wait) in [
        (7010, "x86tool", StartWait::Translating),
        (7011, "nfsprog", StartWait::NotStarted),
        (7012, "smbprog", StartWait::NotStarted),
    ] {
        let mut p = held_orphan(pid, name, true);
        p.wait = wait;
        w.add(p);
    }
    w.observe();
    let v = w.tick(TICK_MS);
    let rows = |list: &[Named]| list.iter().map(|n| (n.pid, n.for_ms)).collect::<Vec<_>>();
    assert_eq!(rows(&v.findings.translating), [(7010, TICK_MS)]);
    assert_eq!(
        rows(&v.findings.not_started),
        [(7011, TICK_MS), (7012, TICK_MS)]
    );
    assert!(v.findings.program_check.is_empty(), "{:?}", v.findings);
    assert_eq!(v.findings.program_check_aterm, (0, 0));
    assert_eq!(
        v.findings.program_check_episode, None,
        "no row the check holds: no episode"
    );
    let lines = v.findings.wire_lines();
    for line in [
        "finding kind=translating pid=7010 exe=x86tool for_ms=2000",
        "finding kind=not-started pid=7011 exe=nfsprog for_ms=2000",
    ] {
        assert!(lines.contains(&line.to_owned()), "{lines:?}");
    }
}

/// THE DYLIB CASE, A HEURISTIC: inside a backed-up episode, a member that started, holds a dylib
/// a build just wrote open and used no CPU since the reading before is named "probably waiting for
/// the same check"; the same member is not named outside an episode, at its first reading, while
/// its CPU moves, with no fresh dylib open, when macOS's program check holds it itself (the
/// kernel's fact stands alone), or when another process took its pid since (its CPU is not the
/// one the reading before kept).
#[test]
fn inside_an_episode_a_member_flat_on_a_fresh_dylib_probably_waits_for_the_same_check() {
    use aterm_vitals::jobs::{Fd, FdKind};
    let dylib = |fresh_dylib| Fd {
        fd: 9,
        kind: FdKind::File {
            regular: true,
            size: 4096,
            dev: 1,
            ino: 4242,
            lf_tail: None,
            fresh_dylib,
        },
    };
    let world = |held: bool, cpu_per_s: u64, fresh: bool| {
        let mut w = World::new();
        let mut trustc = sleeper(401, "trustc");
        trustc.cpu_per_s = cpu_per_s;
        trustc.fds.push(dylib(fresh));
        w.call(1, 300, 400, trustc, None);
        if held {
            for (pid, name) in [(7001, "agy"), (7003, "otool"), (7004, "codex")] {
                w.add(held_orphan(pid, name, true));
            }
        }
        w
    };
    let probable = |v: &JobsView| {
        v.findings
            .probably_program_check
            .iter()
            .map(|n| (n.pid, n.tab))
            .collect::<Vec<_>>()
    };

    let mut w = world(true, 0, true);
    let first = w.observe();
    assert!(
        first.findings.program_check_episode.is_some(),
        "three held: backed up"
    );
    assert_eq!(
        probable(&first),
        [],
        "its first reading: nothing to compare"
    );
    let v = w.tick(TICK_MS);
    assert_eq!(probable(&v), [(401, Some(1))], "{:?}", v.findings);
    assert!(
        v.findings.wire_lines().contains(
            &"finding kind=probably-program-check pid=401 exe=trustc heuristic=fresh-dylib"
                .to_owned()
        ),
        "{:?}",
        v.findings.wire_lines()
    );

    for (why, held, cpu, fresh) in [
        ("outside an episode", false, 0, true),
        ("its CPU moves", true, 50_000_000, true),
        ("no fresh dylib", true, 0, false),
    ] {
        let mut w = world(held, cpu, fresh);
        w.observe();
        let v = w.tick(TICK_MS);
        assert_eq!(probable(&v), [], "{why}");
    }

    let mut w = world(true, 0, true);
    w.p(401).held = true;
    w.observe();
    let v = w.tick(TICK_MS);
    assert_eq!(held_rows(&v)[0].0, 401, "{:?}", v.findings);
    assert_eq!(probable(&v), [], "held at the check itself");

    let mut w = world(true, 0, true);
    w.observe();
    w.p(401).start += 1;
    let v = w.tick(TICK_MS);
    assert_eq!(probable(&v), [], "another process at its pid");
}

#[test]
fn a_detached_job_reading_its_consumers_pipe_serves() {
    let mut w = World::new();
    let mut server = sleeper(401, "mcp-server");
    server.fds.push(pipe_end(0, 0xD1, 0xD2, false, 0x8, 0));
    w.call(1, 300, 400, server, None);
    w.p(300).fds.push(pipe_end(9, 0xD2, 0xD1, true, 0, 0));
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let row = v.row("a400").expect("the server");
    assert_eq!(row.wait.as_ref().map(|w| w.name), Some(WaitName::Serves));
    assert!(
        row.wire().contains("wait=serves peer=300"),
        "{}",
        row.wire()
    );
    assert_eq!(row.verdict, Verdict::Fine);
    assert_eq!(row.phi_milli, Some(0));
}

#[test]
fn without_lane_l7_a_pipe_reads_unnamed_and_the_lane_says_so() {
    let mut w = World::new();
    w.lanes.pipe_flags = false;
    let mut s = sleeper(1000, "sleep");
    s.fds.push(pipe_end(1, 0xA2, 0xA1, true, 0, 0));
    w.run_fg(1, 51, "sleep 600 | cat", s);
    let mut c = sleeper(1001, "cat");
    c.pgid = 1000;
    c.fds.push(pipe_end(0, 0xA1, 0xA2, false, 0x8, 0));
    w.run_bg(1, c);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Unnamed)
    );
    assert_eq!(v.lanes_off, vec!["lane_off:pipe_flags"]);
}

#[test]
fn the_shown_wait_name_changes_after_two_agreeing_ticks() {
    let mut w = World::new();
    w.run_fg(1, 51, "curl x", curl(1000));
    let v = run_to(&mut w, 10_000, |_, _| {});
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Tcp)
    );
    let fds = std::mem::take(&mut w.p(1000).fds);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Tcp)
    );
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Unnamed)
    );
    w.p(1000).fds = fds;
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Unnamed)
    );
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Tcp)
    );
    let _ = tcp_fd(5);
}

#[test]
fn a_reused_pid_is_a_new_job() {
    let mut w = World::new();
    w.call(1, 300, 400, curl(401), None);
    let _ = run_to(&mut w, 600_000, |_, _| {});
    w.exit(401, false);
    w.exit(400, false);
    let _ = w.tick(TICK_MS);
    let mut fresh = curl(401);
    fresh.start += 1;
    w.call(1, 300, 400, fresh, None);
    w.p(400).start += 1;
    let v = w.tick(TICK_MS);
    let live: Vec<_> = v
        .rows
        .iter()
        .filter(|r| r.id == "a400" && r.verdict != Verdict::Exited)
        .collect();
    assert_eq!(live.len(), 1);
    assert!(
        live[0].quiet_ms.is_some_and(|q| q < 10_000),
        "a fresh quiet clock: {}",
        live[0].wire()
    );
    assert_eq!(live[0].leader.1, Some(400 * 1000 + 8));
}

#[test]
fn an_episode_opens_once_and_a_new_one_after_progress() {
    let mut w = World::new();
    w.run_fg(1, 51, "curl x", curl(1000));
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let r = v.row("b51").expect("row");
    assert_eq!(
        (r.verdict, r.episode),
        (Verdict::Stuck, 1),
        "quiet then stuck: one episode"
    );
    w.advance(TICK_MS);
    w.line(1);
    let v = w.observe();
    assert_eq!(v.row("b51").map(|r| r.verdict), Some(Verdict::Fine));
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("b51").map(|r| (r.verdict, r.episode)),
        Some((Verdict::Stuck, 2))
    );
}

#[test]
fn a_completed_job_teaches_its_command_class() {
    let mut w = World::new();
    w.run_fg(1, 5, "make all", sleeper(1000, "make"));
    let _ = run_to(&mut w, 40_000, |w, rel| {
        if rel % 8_000 == 0 && rel > 0 {
            w.line(1);
        }
    });
    w.exit(1000, false);
    w.close_block(1, 0);
    let _ = w.tick(TICK_MS);
    let class = command_class("make all", aterm_vitals::jobs::JobKind::Shell);
    assert_eq!(
        w.jobs.classes().len(),
        1,
        "a job with 3 intervals or more teaches its class"
    );
    assert_eq!(
        w.jobs.classes().mu(&class),
        None,
        "one completed job is not enough"
    );
    for id in 6..8 {
        w.run_fg(
            1,
            id,
            "make all",
            sleeper(1000 + u32::try_from(id).expect("small"), "make"),
        );
        let _ = run_to(&mut w, 40_000, |w, rel| {
            if rel % 8_000 == 0 && rel > 0 {
                w.line(1);
            }
        });
        w.exit(1000 + u32::try_from(id).expect("small"), false);
        w.close_block(1, 0);
        let _ = w.tick(TICK_MS);
    }
    assert_eq!(w.jobs.classes().mu(&class), Some(8_000));
    w.run_fg(1, 9, "make all", sleeper(1100, "make"));
    let v = run_to(&mut w, 20_000, |_, _| {});
    let r = v.row("b9").expect("row");
    assert_eq!(r.mu_from, Some(MuFrom::Class));
    assert_eq!(r.mu_ms, Some(10_000), "the floor under a learned 8 s");
}

#[test]
fn verdict_and_class_words_are_closed_and_share_only_exited() {
    let verdicts: Vec<&str> = Verdict::ALL.iter().map(|v| v.word()).collect();
    let classes: Vec<&str> = Class::ALL.iter().map(|c| c.word()).collect();
    assert_eq!(
        verdicts,
        [
            "fine",
            "waiting-you",
            "parked",
            "quiet",
            "busy",
            "slow",
            "stuck",
            "never-returns",
            "exited"
        ]
    );
    assert_eq!(
        classes,
        [
            "exited",
            "unread-input",
            "running",
            "stopped",
            "thrashing",
            "busy-silent",
            "polling",
            "zombie",
            "waiting"
        ]
    );
    for v in Verdict::ALL {
        assert_eq!(Verdict::from_word(v.word()), Some(v));
    }
    for c in Class::ALL {
        assert_eq!(Class::from_word(c.word()), Some(c));
    }
    assert_eq!(Verdict::from_word("done"), Some(Verdict::Exited));
    let shared: Vec<&&str> = verdicts.iter().filter(|w| classes.contains(w)).collect();
    assert_eq!(shared, [&"exited"]);
    assert_eq!(Verdict::from_word("running"), None);
    assert_eq!(Class::from_word("stuck"), None);
}

#[test]
fn every_command_of_a_block_is_one_job_with_one_id() {
    let mut w = World::new();
    let mut make = sleeper(1000, "make");
    make.cpu_per_s = 900_000_000;
    w.run_fg(1, 51, "make && make test", make);
    let _ = run_to(&mut w, 6_000, |w, _| w.line(1));
    w.exit(1000, false);
    w.tab(1).tpgid = 200;
    let between = w.tick(TICK_MS);
    let mut t = Proc::new(1001, 200, "make");
    t.cpu_per_s = 900_000_000;
    t.sid = 200;
    t.tty = true;
    w.add(t);
    w.tab(1).tpgid = 1001;
    let v = run_to(&mut w, 6_000, |w, _| w.line(1));
    for view in [&between, &v] {
        let rows: Vec<String> = view
            .rows
            .iter()
            .filter(|r| r.id == "b51")
            .map(|r| r.wire())
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
    }
    let row = v.row("b51").expect("the block");
    assert_eq!((row.class, row.verdict), (Class::Running, Verdict::Fine));
    assert!(row.wire().contains(" exe=make "), "{}", row.wire());
    w.exit(1001, false);
    w.close_block(1, 2);
    let v = w.tick(TICK_MS);
    let row = v.row("b51").expect("the exited block");
    assert_eq!((row.verdict, row.exit), (Verdict::Exited, Some(2)));
    assert!(row.wire().contains(" exe=make "), "{}", row.wire());
    assert_eq!(v.rows.len(), 1, "no orphan of the shell");
}

#[test]
fn a_typed_sleep_poll_loop_is_one_polling_job_that_reaches_stuck() {
    let s = scenarios::named("sleeppoll.interactive");
    let t = scenarios::run(&s, None);
    let bad = scenarios::check(&s, &t);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let most = t
        .ticks
        .iter()
        .map(|(_, _, v)| v.rows.len())
        .max()
        .unwrap_or(0);
    assert_eq!(most, 1, "one row for the whole loop, never one per sleep");
}

/// A BLOCK LEARNED AFTER ITS GROUP IN FRONT IS THAT GROUP'S JOB: an engine
/// restored with a command running, or a fork swept before the reader fed its
/// `C`, shows the group in front a tick before its block. The block takes
/// that job over and stays one job, through the next ticks, to its `D`.
#[test]
fn a_block_learned_after_its_group_in_front_is_that_groups_one_job() {
    let mut w = World::new();
    for _ in 0..OWN_MIN {
        w.jobs
            .learn(&command_class("python3 train.py", JobKind::Shell), 9_000);
    }
    w.run_bg(1, sleeper(1300, "Python"));
    w.tab(1).tpgid = 1300;
    let v = w.tick(TICK_MS);
    assert_eq!(v.row("j1300").map(|r| r.kind), Some(JobKind::Shell));
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 14,
        started_ms: at,
        open: true,
        command: "python3 train.py".to_owned(),
        status: None,
        stopped: false,
    });
    for n in 1..=2 {
        let v = w.tick(TICK_MS);
        let live: Vec<&str> = v
            .rows
            .iter()
            .filter(|r| r.verdict != Verdict::Exited)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(live, ["j1300"], "tick {n}: {:?}", v.rows);
        assert_eq!(
            v.row("j1300").and_then(|r| r.mu_from),
            Some(MuFrom::Class),
            "the block's command names it: {:?}",
            v.rows
        );
    }
    w.exit(1300, false);
    w.close_block(1, 2);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("j1300").map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, Some(2)))
    );
}

/// A BACKGROUND WRITER IS JUDGED BY ITS LINES: the mode word is the
/// foreground reader's, at a prompt the line editor's raw one, so the
/// shell's own redraws and a background job's `\r` spinner are no progress
/// of the tab's sole background job, and its lines are.
#[test]
fn a_sole_background_job_is_judged_by_its_lines_under_the_line_editors_raw_word() {
    let mut w = World::new();
    w.run_bg(1, sleeper(1400, "curl"));
    let v = run_to(&mut w, 30_000, |w, _| {
        w.tty_mode(1, false, false);
        w.cr(1);
    });
    let row = v.row("j1400").expect("the background job");
    assert_ne!(row.class, Class::Running, "{}", row.wire());
    let v = run_to(&mut w, 2_000, |w, _| w.line(1));
    let row = v.row("j1400").expect("the background job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
}

/// A LONE BACKGROUND JOB IS NOT CREDITED WITH THE SHELL'S LINES: a tick in
/// which the shell wrote a mark of its own (a prompt, a command that ran and
/// ended between two ticks) gives the tab's lines to no background job; a
/// tick with lines and no mark gives them to it.
#[test]
fn a_lone_background_job_is_not_credited_with_a_tick_the_shell_wrote_in() {
    let mut w = World::new();
    w.run_bg(1, sleeper(1400, "curl"));
    let v = run_to(&mut w, 30_000, |_, _| {});
    let q = v.row("j1400").and_then(|r| r.quiet_ms).expect("quiet");
    w.advance(TICK_MS);
    w.line(1);
    w.mark(1);
    let v = w.observe();
    let row = v.row("j1400").expect("the background job");
    assert!(row.quiet_ms.is_some_and(|n| n > q), "{}", row.wire());
    w.advance(TICK_MS);
    w.line(1);
    let v = w.observe();
    let row = v.row("j1400").expect("the background job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
}

/// A STOP AND `fg` NO TICK SAW KEEP THE ONE JOB: the tab's block is the `fg`
/// block by the next tick, the stopped one ended in a stop, and the job's own
/// group is in front again. It keeps its id and quiet clock (the shell's own
/// lines that tick, its notice of `fg` after `fg`'s `C` among them, are none
/// of its progress), never exits with the stop's status nor leaves its
/// program an orphan.
#[test]
fn a_stop_and_fg_no_tick_saw_keep_the_one_job_and_its_quiet_clock() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.p(shell).counters.cpu_ns = 5_000_000_000;
    w.p(shell).counters.logical_writes = Some(64 << 20);
    w.run_fg(1, 12, "curl x", curl(1200));
    let v = run_to(&mut w, 60_000, |_, _| {});
    let q = v.row("b12").and_then(|r| r.quiet_ms).expect("quiet");
    w.stop_block(1);
    w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1200;
    for n in 0..2 {
        w.advance(TICK_MS);
        if n == 0 {
            // `fg`'s `C`, the shell's newest mark, then its notice of `fg`.
            let c = w.at - 10;
            w.tab(1)
                .tab
                .block
                .as_mut()
                .expect("the fg block")
                .started_ms = c;
            w.tab(1).tab.shell_ms = Some(c);
            w.line(1);
        }
        let v = w.observe();
        assert!(v.row("b13").is_none(), "{:?}", v.rows);
        assert!(
            v.rows.iter().all(|r| !r.id.starts_with('o')),
            "{:?}",
            v.rows
        );
        let row = v.row("b12").expect("the one job");
        assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
        assert!(row.quiet_ms.is_some_and(|n| n > q), "{}", row.wire());
    }
}

/// THE NEXT COMMAND TYPED AHEAD IS ITS OWN BLOCK'S JOB: a tick that read the
/// tab before `D` gave the group the next command put in front to the block
/// that was ending. Once the next block is the tab's, that block hands it
/// over: the ended block's job exits with its status (or parks, when its
/// command was stopped) and the next command is the next block's job, no
/// orphan.
#[test]
fn the_next_command_typed_ahead_is_its_own_blocks_job() {
    for stopped in [false, true] {
        let mut w = World::new();
        w.run_fg(1, 7, "make", sleeper(1000, "make"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        if stopped {
            w.p(1000).state = ProcState::Stopped;
        } else {
            w.exit(1000, false);
        }
        w.run_bg(1, sleeper(1001, "less"));
        w.tab(1).tpgid = 1001;
        let _ = w.tick(TICK_MS);
        if stopped {
            w.stop_block(1);
        } else {
            w.close_block(1, 0);
        }
        w.run_fg(1, 8, "less x", sleeper(1002, "unused"));
        w.exit(1002, false);
        w.tab(1).tpgid = 1001;
        for _ in 0..2 {
            let v = w.tick(TICK_MS);
            let ended = v.row("b7").map(|r| (r.kind, r.verdict, r.exit));
            if stopped {
                assert!(
                    ended.is_some_and(|(k, v, _)| k == JobKind::ShellBg && v != Verdict::Exited),
                    "{:?}",
                    v.rows
                );
            } else {
                assert_eq!(
                    ended,
                    Some((JobKind::Shell, Verdict::Exited, Some(0))),
                    "{:?}",
                    v.rows
                );
            }
            let row = v.row("b8").expect("the next command's job");
            assert_ne!(row.verdict, Verdict::Exited, "{stopped}: {}", row.wire());
            assert!(row.wire().contains(" exe=less "), "{}", row.wire());
            assert!(
                v.rows.iter().all(|r| !r.id.starts_with('o')),
                "{:?}",
                v.rows
            );
        }
    }
}

/// A process the shell started in its own group (a process substitution:
/// `exec > >(tee log)`, `less <(tail -f log)`), kept by an orphan or a parked
/// job.
fn in_the_shells_group(w: &mut World, pid: u32, name: &str) {
    let shell = w.tab(1).tab.shell_pid;
    let mut p = Proc::new(pid, shell, name);
    p.pgid = shell;
    p.sid = shell;
    p.tty = true;
    w.add(p);
}

/// A SURVIVOR IN THE SHELL'S OWN GROUP NEVER HIDES THE JOB `fg` RESUMES: the
/// `tee` an earlier block left as an orphan is in every block's unit, and the
/// block `fg` opens resumes the parked job by the group in front.
#[test]
fn a_survivor_in_the_shells_own_group_never_hides_the_job_fg_resumes() {
    let mut w = World::new();
    in_the_shells_group(&mut w, 1100, "tee");
    w.run_fg(1, 11, "make", sleeper(1000, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.close_block(1, 0);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let v = run_to(&mut w, 4_000, |_, _| {});
    assert!(v.rows.iter().any(|r| r.id.starts_with('o')), "{:?}", v.rows);
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1200;
    w.p(1200).state = ProcState::Sleeping;
    let v = w.tick(TICK_MS);
    assert!(v.row("b13").is_none(), "{:?}", v.rows);
    let row = v.row("b12").expect("the resumed job");
    assert_eq!(row.kind, JobKind::Shell, "{}", row.wire());
}

/// A debugger's child in a group of its own, in tab 1's terminal.
fn inferior(w: &mut World, pid: u32, gdb: u32, state: ProcState) {
    let shell = w.tab(1).tab.shell_pid;
    let mut p = Proc::new(pid, gdb, "srv");
    p.sid = shell;
    p.tty = true;
    p.state = state;
    w.add(p);
}

/// A DEBUGGER'S CHILD IT PUTS IN FRONT IS ITS BLOCK'S: gdb's inferior, first
/// seen stopped in a group gdb made for it, is gdb's job's, and gets the
/// terminal at `continue`: the block's job (gdb) keeps it rather than giving
/// way to it.
#[test]
fn a_debuggers_child_it_puts_in_front_is_its_blocks() {
    let mut w = World::new();
    w.run_fg(1, 2, "gdb ./srv", sleeper(1200, "gdb"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    inferior(&mut w, 1300, 1200, ProcState::Stopped);
    let v = w.tick(TICK_MS);
    assert_eq!(
        (v.row("b2").map(|r| r.members), v.row("j1300").is_none()),
        (Some(3), true),
        "{:?}",
        v.rows
    );
    w.p(1300).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1300;
    for _ in 0..2 {
        let v = w.tick(TICK_MS);
        let row = v.row("b2").expect("gdb's job");
        assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
        assert_eq!((row.members, row.pgid), (3, 1300), "{}", row.wire());
        assert!(v.row("j1300").is_none(), "{:?}", v.rows);
    }
}

#[test]
fn a_tracked_command_beyond_discoverys_cap_keeps_its_live_block() {
    for siblings in [4094u32, 4095] {
        let mut w = World::new();
        w.run_bg(1, sleeper(300, "nested job"));
        w.run_fg(1, 1, "owner", sleeper(9000, "owner"));
        let v = w.observe();
        assert_eq!(v.row("b1").map(|r| r.members), Some(2));
        assert!(v.row("j300").is_some());

        // Lower pids can be allocated after a remembered command. Its identity and
        // actual shell parent stay unchanged while this pass's discovery fills up.
        for pid in 1000..1000 + siblings {
            let mut p = sleeper(pid, "nested stage");
            p.pgid = 300;
            w.run_bg(1, p);
        }
        w.tab(1).tpgid = 300;
        for _ in 0..2 {
            let v = w.tick(TICK_MS);
            let row = v.row("b1").expect("the owner's live block");
            assert_ne!(row.verdict, Verdict::Exited, "{siblings}: {}", row.wire());
            assert_eq!(
                row.members,
                usize::try_from(siblings).expect("the bounded sibling count") + 3,
                "{siblings}: {}",
                row.wire()
            );
            assert!(v.row("j300").is_none(), "{siblings}: {:?}", v.rows);
            assert!(
                v.rows.iter().all(|r| r.kind != JobKind::Orphan),
                "{siblings}: {:?}",
                v.rows
            );
        }
    }
}

#[test]
fn an_uncertain_parent_chain_defers_transfer_until_a_later_read_proves_it() {
    for cycle in [false, true] {
        for adopted in [false, true] {
            let mut w = World::new();
            w.run_bg(1, sleeper(300, "nested job"));
            w.run_fg(1, 1, "owner", sleeper(9000, "owner"));
            let _ = w.observe();
            w.p(9000).ppid = 9001;
            if cycle {
                let mut p = Proc::new(9001, 9000, "unresolved parent");
                p.pgid = 9000;
                p.sid = 200;
                p.tty = true;
                w.add(p);
            }
            w.tab(1).tpgid = 300;
            for _ in 0..2 {
                let v = w.tick(TICK_MS);
                let row = v.row("b1").expect("the unresolved owner's block");
                assert_ne!(row.verdict, Verdict::Exited, "{cycle}: {}", row.wire());
                assert_eq!(row.pgid, 9000, "{cycle}: {}", row.wire());
                assert_eq!(row.members, 2, "{cycle}: {}", row.wire());
                let row = v.row("j300").expect("no unproved ownership transfer");
                assert_eq!(row.kind, JobKind::ShellBg, "{cycle}: {}", row.wire());
            }

            w.procs.remove(&9001);
            w.p(9000).ppid = if adopted { 1 } else { 200 };
            let v = w.tick(TICK_MS);
            let row = v.row("b1").expect("the original block's row");
            if adopted {
                // Proven adoption follows the ordinary one-pass foreground deferral.
                assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
                assert_eq!(v.row("j300").map(|r| r.kind), Some(JobKind::Shell));
                let v = w.tick(TICK_MS);
                assert_eq!(v.row("b1").map(|r| r.verdict), Some(Verdict::Exited));
                assert_eq!(v.row("j300").map(|r| r.kind), Some(JobKind::Shell));
                let v = w.tick(TICK_MS);
                assert!(
                    v.rows
                        .iter()
                        .any(|r| r.kind == JobKind::Orphan && r.exe == "owner"),
                    "{:?}",
                    v.rows
                );
            } else {
                assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
                assert_eq!(row.members, 3, "{}", row.wire());
                assert!(v.row("j300").is_none(), "{:?}", v.rows);
            }
        }
    }
}

/// A DEBUGGER'S TRACED STOP IS NO STOP OF ITS JOB: the inferior stopped at a
/// breakpoint leaves gdb in front as the block's last group, so a Ctrl-Z of
/// gdb and `fg` no tick saw resume the one job.
#[test]
fn a_debuggers_traced_stop_keeps_it_the_group_fg_resumes() {
    let mut w = World::new();
    w.run_fg(1, 2, "gdb ./srv", sleeper(1200, "gdb"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    inferior(&mut w, 1300, 1200, ProcState::Sleeping);
    w.tab(1).tpgid = 1300;
    let _ = w.tick(TICK_MS);
    w.p(1300).state = ProcState::Stopped;
    w.tab(1).tpgid = 1200;
    let _ = w.tick(TICK_MS);
    w.stop_block(1);
    w.run_fg(1, 3, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1200;
    let v = w.tick(TICK_MS);
    assert!(v.row("b3").is_none(), "{:?}", v.rows);
    let row = v.row("b2").expect("the one job");
    assert_eq!(row.kind, JobKind::Shell, "{}", row.wire());
}

/// A PARKED JOB'S PROCESS IN THE SHELL'S OWN GROUP NEVER TAKES THE NEXT
/// BLOCK: `less <(tail -f log)` parked by Ctrl-Z keeps the `tail` the shell
/// started for it, and the next command is its own block's job.
#[test]
fn a_parked_jobs_process_in_the_shells_own_group_never_takes_the_next_block() {
    let mut w = World::new();
    in_the_shells_group(&mut w, 1100, "tail");
    w.run_fg(1, 20, "less <(tail -f app.log)", sleeper(1200, "less"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let v = run_to(&mut w, 10_000, |_, _| {});
    assert_eq!(
        v.row("b20").map(|r| r.verdict),
        Some(Verdict::Parked),
        "{:?}",
        v.rows
    );
    w.run_fg(1, 21, "make", sleeper(1300, "make"));
    let v = run_to(&mut w, 4_000, |_, _| {});
    let row = v.row("b21").expect("make's own job");
    assert!(row.wire().contains(" exe=make "), "{}", row.wire());
    assert_eq!(
        v.row("b20").map(|r| r.kind),
        Some(JobKind::ShellBg),
        "{:?}",
        v.rows
    );
}

/// A STOPPED MEMBER OUT OF FRONT IS NO HELD STOP: job control took the
/// terminal from it. A Ctrl-Z'd job stays parked while the next command holds
/// the terminal, and a member stopped while another of its job's groups runs
/// in front (gdb's inferior at a breakpoint, the command bash moved on to
/// past a stopped one of its list) is no stop of that job's, which is judged
/// by what runs in front. None reads quiet or stuck however long it lasts.
/// A stage stopped in the group in front (`kill -STOP` of `a` in `a | b`)
/// holds it. A group the program in front made for itself (ninja's step)
/// stopped at the terminal is that program's job's, which waits on it and
/// reads quiet (its wait is capped there). A pipeline whose leader
/// exited before its Ctrl-Z stays parked by its group, and a `cmd &` job the
/// terminal stopped (`SIGTTIN`) is the shell's to `fg`: parked.
#[test]
fn a_stopped_member_out_of_front_is_no_held_stop() {
    for setup in [
        "parked beside make",
        "gdb at a breakpoint",
        "bash past a stop",
        "a stage stopped in front",
        "a group make made",
        "a pipeline whose leader exited",
        "a background job the terminal stopped",
    ] {
        let mut w = World::new();
        let id = match setup {
            "parked beside make" => {
                w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.p(1200).state = ProcState::Stopped;
                w.stop_block(1);
                let _ = w.tick(TICK_MS);
                w.run_fg(1, 13, "make", sleeper(1300, "make"));
                "b12"
            }
            "gdb at a breakpoint" => {
                w.run_fg(1, 2, "gdb ./srv", sleeper(1200, "gdb"));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                inferior(&mut w, 1300, 1200, ProcState::Sleeping);
                w.tab(1).tpgid = 1300;
                let _ = w.tick(TICK_MS);
                w.p(1300).state = ProcState::Stopped;
                w.tab(1).tpgid = 1200;
                "b2"
            }
            "bash past a stop" => {
                w.run_fg(1, 1, "vim a; vim b", sleeper(1010, "vim"));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.p(1010).state = ProcState::Stopped;
                w.run_bg(1, sleeper(1011, "vim"));
                w.tab(1).tpgid = 1011;
                "b1"
            }
            "a stage stopped in front" => {
                w.run_fg(1, 3, "a | b", sleeper(1300, "a"));
                let mut b = sleeper(1301, "b");
                b.pgid = 1300;
                w.run_bg(1, b);
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.p(1300).state = ProcState::Stopped;
                "b3"
            }
            "a group make made" => {
                w.run_fg(1, 7, "make", sleeper(1300, "make"));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                inferior(&mut w, 1310, 1300, ProcState::Stopped);
                "b7"
            }
            "a background job the terminal stopped" => {
                w.run_bg(1, curl(1500));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.p(1500).state = ProcState::Stopped;
                w.run_fg(1, 13, "make", sleeper(1300, "make"));
                "j1500"
            }
            _ => {
                w.run_fg(1, 5, "cat log | less", sleeper(1300, "cat"));
                let mut less = sleeper(1301, "less");
                less.pgid = 1300;
                w.run_bg(1, less);
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.exit(1300, false);
                let _ = w.tick(TICK_MS);
                w.p(1301).state = ProcState::Stopped;
                w.stop_block(1);
                let _ = w.tick(TICK_MS);
                w.run_fg(1, 6, "make", sleeper(1400, "make"));
                "b5"
            }
        };
        // A build runs in the terminal's cooked mode.
        let cooked = setup == "a group make made";
        w.tty_mode(1, cooked, cooked);
        let v = run_to(&mut w, 600_000, |_, _| {});
        let row = v.row(id).expect("the job");
        if setup == "a group make made" {
            assert_eq!(
                (row.verdict, row.members, v.row("j1310").is_none()),
                (Verdict::Quiet, 3, true),
                "{setup}: {:?}",
                v.rows
            );
        } else if matches!(
            setup,
            "parked beside make"
                | "a pipeline whose leader exited"
                | "a background job the terminal stopped"
        ) {
            assert_eq!(row.verdict, Verdict::Parked, "{setup}: {}", row.wire());
        } else if setup == "a stage stopped in front" {
            assert_eq!(
                (row.class, row.stop.as_ref().map(|s| s.parked)),
                (Class::Stopped, Some(false)),
                "{setup}: {}",
                row.wire()
            );
        } else {
            assert_ne!(row.class, Class::Stopped, "{setup}: {}", row.wire());
            assert!(
                !matches!(row.verdict, Verdict::Quiet | Verdict::Stuck),
                "{setup}: {}",
                row.wire()
            );
        }
    }
}

/// A `fg` TYPED AHEAD AND READ BEFORE THE LAST BLOCK'S `D` RESUMES THE
/// PARKED JOB: a pass reads the tab while block 13 (`make`, over, swept or
/// not before the pass read the processes, or never seen at all) is still
/// open and the parked vim already in front. Block 13's job ends with its
/// own status once `D` is read, also when a pass reads make's `D` before
/// `fg`'s `C`, and the resumed job is `fg`'s block's, keeping its id, with no
/// orphan beside it, also when it is Ctrl-Z'd again before the next pass. One
/// that quits before the next pass never takes make's status, and make's job
/// then takes none either (a command list's `D` would read the same); one
/// whose `fg` writes no block (a zle widget's) ends with no status.
#[test]
fn a_typed_ahead_fg_read_before_the_last_blocks_d_resumes_the_parked_job() {
    for case in [
        "exited",
        "swept",
        "quits",
        "unseen",
        "straddle",
        "exited straddle",
        "widget",
        "stops",
    ] {
        let unseen = matches!(case, "unseen" | "straddle" | "widget");
        let mut w = World::new();
        w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.p(1200).state = ProcState::Stopped;
        w.stop_block(1);
        let _ = w.tick(TICK_MS);
        if unseen {
            w.run_fg(1, 13, "make", sleeper(1300, "make"));
            w.exit(1300, false);
        } else {
            w.run_fg(1, 13, "make", sleeper(1300, "make"));
            let _ = run_to(&mut w, 4_000, |_, _| {});
            if case == "swept" {
                w.unread.insert(1300);
            } else {
                w.exit(1300, false);
            }
        }
        w.p(1200).state = ProcState::Sleeping;
        w.tab(1).tpgid = 1200;
        let v = w.tick(TICK_MS);
        let row = v.row("b12").expect("the resumed job");
        assert_eq!(row.kind, JobKind::Shell, "{case}: {}", row.wire());
        w.exit(1300, false);
        w.unread.clear();
        w.close_block(1, 2);
        if case.ends_with("straddle") || case == "widget" {
            w.tab(1).tpgid = 1200;
            let _ = w.tick(TICK_MS);
        }
        if case == "widget" {
            for _ in 0..3 {
                let v = w.tick(TICK_MS);
                let row = v.row("b12").expect("the resumed job");
                assert_eq!(
                    (row.kind, row.verdict == Verdict::Exited, row.exit),
                    (JobKind::Shell, false, None),
                    "{case}: {}",
                    row.wire()
                );
                assert!(v.row("o1200").is_none(), "{case}: {:?}", v.rows);
            }
            w.exit(1200, false);
            let v = w.tick(TICK_MS);
            assert_eq!(
                v.row("b12").map(|r| (r.verdict, r.exit)),
                Some((Verdict::Exited, None)),
                "{case}: {:?}",
                v.rows
            );
            continue;
        }
        w.run_fg(1, 14, "fg", sleeper(1201, "unused"));
        w.exit(1201, false);
        w.tab(1).tpgid = 1200;
        if case == "quits" {
            w.exit(1200, false);
            w.close_block(1, 0);
        }
        if case == "stops" {
            w.p(1200).state = ProcState::Stopped;
            w.stop_block(1);
        }
        for _ in 0..2 {
            let v = w.tick(TICK_MS);
            assert!(v.row("b14").is_none(), "{case}: {:?}", v.rows);
            assert!(v.row("o1200").is_none(), "{case}: {:?}", v.rows);
            if !unseen {
                assert_eq!(
                    v.row("b13").map(|r| (r.verdict, r.exit)),
                    Some((Verdict::Exited, (case != "quits").then_some(2))),
                    "{case}: {:?}",
                    v.rows
                );
            }
            let row = v.row("b12").expect("the resumed job");
            if case == "quits" {
                assert_ne!(row.exit, Some(2), "{case}: {}", row.wire());
            } else if case == "stops" {
                assert_eq!(
                    (row.kind, row.class),
                    (JobKind::ShellBg, Class::Stopped),
                    "{case}: {}",
                    row.wire()
                );
            } else {
                assert_eq!(
                    (row.kind, row.verdict == Verdict::Exited, row.exit),
                    (JobKind::Shell, false, None),
                    "{case}: {}",
                    row.wire()
                );
            }
        }
    }
}

/// A COMMAND LIST THAT ENDS IN `fg` GIVES WAY PAST A SURVIVOR ITS RECIPE
/// LEFT: `make test && fg` started a server with `&` in make's group, which
/// launchd adopted when make exited, so nothing of the block's own command
/// lives on in the shell's tree. The parked job takes the block, and the
/// server is an orphan's.
#[test]
fn a_command_list_ending_in_fg_gives_way_past_a_survivor_its_recipe_left() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "make test && fg", sleeper(1300, "make"));
    let mut server = Proc::new(1301, 1300, "redis-server");
    server.pgid = 1300;
    server.sid = shell;
    server.tty = true;
    w.add(server);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    w.p(1301).ppid = 1;
    w.p(1200).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1200;
    let v = run_to(&mut w, 3 * TICK_MS, |_, _| {});
    let row = v.row("b12").expect("the resumed job");
    assert_eq!(
        (row.kind, row.members),
        (JobKind::Shell, 2),
        "{}",
        row.wire()
    );
    assert!(
        v.rows
            .iter()
            .any(|r| r.id.starts_with('o') && r.wire().contains(" exe=redis-server ")),
        "{:?}",
        v.rows
    );
    w.exit(1200, false);
    w.close_block(1, 1);
    let v = w.tick(TICK_MS);
    assert_eq!(v.row("b12").map(|r| r.exit), Some(Some(1)), "{:?}", v.rows);
    assert_eq!(v.row("b13").map(|r| r.exit), Some(None), "{:?}", v.rows);
}

/// `fg %1` READ BEFORE A STOP'S `D` NEVER MERGES ITS JOB INTO THE STOPPED
/// ONE: a pass reads the tab while vim's block is still open, vim stopped
/// behind it, and the job `fg %1` resumes already in front. Vim's job keeps
/// its block until `D` parks it, and the resumed job is `fg %1`'s block's,
/// keeping its id.
#[test]
fn fg_of_another_job_read_before_a_stops_d_never_merges_it_into_the_stopped_one() {
    let mut w = World::new();
    w.run_fg(1, 11, "curl x", curl(1100));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1100).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.p(1100).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1100;
    let v = w.tick(TICK_MS);
    assert!(
        v.row("b11").is_some_and(|r| r.verdict != Verdict::Exited),
        "{:?}",
        v.rows
    );
    assert_eq!(v.row("b12").map(|r| r.pgid), Some(1200), "{:?}", v.rows);
    w.stop_block(1);
    w.run_fg(1, 13, "fg %1", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1100;
    let v = w.tick(TICK_MS);
    assert!(v.row("b13").is_none(), "{:?}", v.rows);
    assert_eq!(
        v.row("b11").map(|r| (r.kind, r.verdict == Verdict::Exited)),
        Some((JobKind::Shell, false)),
        "{:?}",
        v.rows
    );
    assert_eq!(
        v.row("b12").map(|r| (r.kind, r.verdict)),
        Some((JobKind::ShellBg, Verdict::Parked)),
        "{:?}",
        v.rows
    );
}

/// BASH MOVING ON PAST A STOPPED COMMAND KEEPS THE NEXT ONE ITS BLOCK'S:
/// bash sends one block per line, so after a Ctrl-Z of `vim a` in
/// `vim a; vim b` the block stays open with `vim b` in front. Seen there at
/// two passes, `vim b` is the block's last group in front though `vim a` is
/// stopped behind it, so a Ctrl-Z of `vim b` and `fg` no tick saw resume the
/// one job.
#[test]
fn bash_moving_on_past_a_stopped_command_keeps_the_next_one_its_blocks() {
    let mut w = World::new();
    w.run_fg(1, 1, "vim a; vim b", sleeper(1010, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1010).state = ProcState::Stopped;
    w.run_bg(1, sleeper(1011, "vim"));
    w.tab(1).tpgid = 1011;
    let _ = w.tick(TICK_MS);
    let _ = w.tick(TICK_MS);
    w.p(1011).state = ProcState::Stopped;
    w.stop_block(1);
    w.run_fg(1, 2, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1011;
    w.p(1011).state = ProcState::Sleeping;
    let v = w.tick(TICK_MS);
    assert!(v.row("b2").is_none(), "{:?}", v.rows);
    let row = v.row("b1").expect("the one job");
    assert_eq!(
        (row.kind, row.verdict == Verdict::Exited),
        (JobKind::Shell, false),
        "{}",
        row.wire()
    );
}

/// A JOB BASH BRINGS FORWARD IN A LIST PAST A STOP IS RESUMED: bash sends
/// one block per line, so after a Ctrl-Z of `make` in `make; fg %1` the
/// block stays open with the parked vim in front. Make's job parks, never
/// taking vim's lines, and vim's job takes the block and its lines from the
/// pass after the one that resumed it.
#[test]
fn a_job_bash_brings_forward_in_a_list_past_a_stop_is_resumed() {
    let mut w = World::new();
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "make; fg %1", sleeper(1300, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1300).state = ProcState::Stopped;
    w.p(1200).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1200;
    for pass in 0..5 {
        w.advance(TICK_MS);
        w.line(1);
        let v = w.observe();
        assert!(
            v.row("b13").is_none_or(|r| r.class != Class::Running),
            "{pass}: {:?}",
            v.rows
        );
        if pass > 0 {
            assert_eq!(
                v.row("b12").map(|r| r.class),
                Some(Class::Running),
                "{pass}: {:?}",
                v.rows
            );
        }
    }
    let v = run_to(&mut w, 4_000, |w, _| w.line(1));
    let row = v.row("b12").expect("vim's job");
    assert_eq!(
        (row.kind, row.class),
        (JobKind::Shell, Class::Running),
        "{}",
        row.wire()
    );
    assert_eq!(
        v.row("b13").map(|r| (r.kind, r.verdict)),
        Some((JobKind::ShellBg, Verdict::Parked)),
        "{:?}",
        v.rows
    );
}

/// A JOB A COMMAND LIST BRINGS FORWARD KEEPS WHAT ITS GROUP FORKED:
/// `./build.sh &` forks a step into its group while `pytest && fg` hands it
/// the terminal; the step is the build's, and pytest's row keeps its name.
#[test]
fn a_job_a_command_list_brings_forward_keeps_what_its_group_forked() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_bg(1, sleeper(1200, "build.sh"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.run_fg(1, 13, "pytest && fg", sleeper(1300, "pytest"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    let mut step = Proc::new(1201, 1200, "cc");
    step.pgid = 1200;
    step.sid = shell;
    step.tty = true;
    w.add(step);
    w.tab(1).tpgid = 1200;
    let v = run_to(&mut w, 2 * TICK_MS, |_, _| {});
    let row = v.row("j1200").expect("the build");
    assert_eq!(
        (row.kind, row.members),
        (JobKind::Shell, 3),
        "{}",
        row.wire()
    );
    assert!(
        v.row("b13")
            .is_some_and(|r| r.wire().contains(" exe=pytest ")),
        "{:?}",
        v.rows
    );
}

/// A JOB PARKED IN TWO GROUPS TAKES THE BLOCK THAT RESUMES ONE: bash's
/// `vim a; vim b`, both stopped, parks one job keyed by vim a's group, and
/// `cargo build; fg` resumes vim b; the job takes the block while vim a stays
/// stopped beside it.
#[test]
fn a_job_parked_in_two_groups_takes_the_block_that_resumes_one() {
    let mut w = World::new();
    w.run_fg(1, 1, "vim a; vim b", sleeper(1010, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1010).state = ProcState::Stopped;
    w.run_bg(1, sleeper(1011, "vim"));
    w.tab(1).tpgid = 1011;
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1011).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.run_fg(1, 2, "cargo build; fg", sleeper(1300, "cargo"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    w.p(1011).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1011;
    let v = run_to(&mut w, 3 * TICK_MS, |_, _| {});
    let row = v.row("b1").expect("the vims' job");
    assert_eq!(
        (row.kind, row.consumer),
        (JobKind::Shell, Consumer::PersonFg),
        "{}",
        row.wire()
    );
    assert_eq!(
        v.row("b2").map(|r| r.verdict),
        Some(Verdict::Exited),
        "{:?}",
        v.rows
    );
}

/// A JOB A COMMAND LIST BRINGS FORWARD THAT QUITS AT ONCE LEAVES MAKE NO
/// STATUS: `make && fg` resumes vim, which quits (`:cq`) before a pass gave it
/// the block, and the list's `D` is `fg`'s, not make's, also when a pass
/// reads vim gone before the reader feeds that `D`, when the job keeps a
/// process substitution in the shell's group (`vim <(tail -f log)`), and when
/// a child it started in its group lives on adopted (`fg` waits for no
/// grandchild). vim's own row ends with it. A swept zombie and a different
/// process that reused vim's PID prove no live vim either.
#[test]
fn a_job_a_command_list_brings_forward_that_quits_at_once_leaves_make_no_status() {
    for (between, extra) in [
        (false, ""),
        (true, ""),
        (false, "subst"),
        (true, "subst"),
        (false, "child"),
    ] {
        for fate in ["gone", "zombie", "reused"] {
            let mut w = World::new();
            w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
            if extra == "subst" {
                in_the_shells_group(&mut w, 1100, "tail");
            }
            if extra == "child" {
                let shell = w.tab(1).tab.shell_pid;
                let mut c = Proc::new(1210, 1200, "tailwind");
                c.pgid = 1200;
                c.sid = shell;
                c.tty = true;
                w.add(c);
            }
            let _ = run_to(&mut w, 4_000, |_, _| {});
            w.p(1200).state = ProcState::Stopped;
            w.stop_block(1);
            let _ = w.tick(TICK_MS);
            w.run_fg(1, 13, "make && fg", sleeper(1300, "make"));
            let _ = run_to(&mut w, 4_000, |_, _| {});
            w.exit(1300, false);
            w.p(1200).state = ProcState::Sleeping;
            w.tab(1).tpgid = 1200;
            let _ = w.tick(TICK_MS);
            w.exit(1200, fate == "zombie");
            if extra == "child" {
                w.p(1210).ppid = 1;
            }
            if fate == "reused" {
                let mut other = Proc::new(1200, 1, "other");
                other.start += 1;
                other.pgid = 8000;
                w.add(other);
            }
            if between {
                w.tab(1).tpgid = w.tab(1).tab.shell_pid;
                let _ = w.tick(TICK_MS);
            }
            w.close_block(1, 1);
            let v = w.tick(TICK_MS);
            assert_eq!(
                (
                    v.row("b13").map(|r| (r.verdict, r.exit)),
                    v.row("b12").map(|r| r.verdict)
                ),
                (Some((Verdict::Exited, None)), Some(Verdict::Exited)),
                "{between} {extra} {fate}: {:?}",
                v.rows
            );
        }
    }
}

/// A COMMAND LINE THE SHELL RUNS ITSELF TAKES ITS LINES: past `sleep 1`, a
/// builtin loop (`while :; do echo tick; done`) keeps the terminal with the
/// shell, and its block's job is judged by the lines it prints, also over
/// passes that could not read the shell's terminal group.
#[test]
fn a_command_line_the_shell_runs_itself_takes_its_lines() {
    for unread in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        w.run_fg(
            1,
            3,
            "sleep 1; while :; do echo tick; done",
            sleeper(1300, "sleep"),
        );
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.exit(1300, false);
        w.tab(1).tpgid = shell;
        if unread {
            w.unread.insert(shell);
        }
        let v = run_to(&mut w, 10_000, |w, _| w.line(1));
        assert_eq!(
            v.row("b3").map(|r| r.class),
            Some(Class::Running),
            "{unread}: {:?}",
            v.rows
        );
    }
}

/// A STOP'S `D` READ BEFORE THE `fg` THAT RESUMED IT PARKS NOTHING: vim,
/// Ctrl-Z'd and resumed by a `fg` no pass saw, is in front again when a pass
/// reads the stop's `D` but not yet `fg`'s `C`; its job runs on in front and
/// takes `fg`'s block, whose shell's notice of `fg` is no line of vim's. One
/// whose `fg` writes no block (a zle widget's) is its group's job: what vim
/// forks joins it, a second Ctrl-Z parks it, and it ends when vim does, also
/// while a process substitution of its command line lives on.
#[test]
fn a_stops_d_read_before_the_fg_that_resumed_it_parks_nothing() {
    for widget in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        if widget {
            in_the_shells_group(&mut w, 1100, "tail");
        }
        w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.stop_block(1);
        w.tab(1).tpgid = 1200;
        if widget {
            let mut sh = Proc::new(1210, 1200, "sh");
            sh.pgid = 1200;
            sh.sid = shell;
            sh.tty = true;
            w.add(sh);
        }
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b12").map(|r| (r.kind, r.class == Class::Stopped)),
            Some((JobKind::Shell, false)),
            "{widget}: {:?}",
            v.rows
        );
        if widget {
            let _ = w.tick(TICK_MS);
            w.p(1200).state = ProcState::Stopped;
            w.p(1210).state = ProcState::Stopped;
            w.tab(1).tpgid = shell;
            let v = w.tick(TICK_MS);
            assert!(v.row("j1200").is_none(), "{:?}", v.rows);
            assert_eq!(
                v.row("b12").map(|r| (r.kind, r.class)),
                Some((JobKind::ShellBg, Class::Stopped)),
                "{:?}",
                v.rows
            );
            w.exit(1210, false);
            w.exit(1200, false);
            let v = w.tick(TICK_MS);
            assert_eq!(
                v.row("b12").map(|r| (r.verdict, r.exit)),
                Some((Verdict::Exited, None)),
                "{widget}: {:?}",
                v.rows
            );
            continue;
        }
        w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
        w.exit(1201, false);
        w.tab(1).tpgid = 1200;
        let _ = w.tick(TICK_MS);
        w.advance(500);
        w.line(1);
        w.advance(TICK_MS - 500);
        let v = w.observe();
        assert!(v.row("b13").is_none(), "{:?}", v.rows);
        assert_eq!(
            v.row("b12")
                .map(|r| (r.kind, r.verdict == Verdict::Exited, r.class)),
            Some((JobKind::Shell, false, Class::Waiting)),
            "{:?}",
            v.rows
        );
    }
}

/// A COMMAND LIST THAT MOVES ON PAST THE JOB IT BROUGHT FORWARD KEEPS ITS
/// STATUS: bash's `make; fg %1; git status`, with vim Ctrl-Z'd again before a
/// pass gave it the block, ends with git's status on the list's row, also
/// when git ran between two passes.
#[test]
fn a_command_list_that_moves_on_past_the_job_it_brought_forward_keeps_its_status() {
    for seen in [true, false] {
        let mut w = World::new();
        w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.p(1200).state = ProcState::Stopped;
        w.stop_block(1);
        let _ = w.tick(TICK_MS);
        w.run_fg(1, 13, "make; fg %1; git status", sleeper(1300, "make"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.exit(1300, false);
        w.p(1200).state = ProcState::Sleeping;
        w.tab(1).tpgid = 1200;
        let _ = w.tick(TICK_MS);
        w.p(1200).state = ProcState::Stopped;
        if seen {
            w.run_bg(1, sleeper(1400, "git"));
            w.tab(1).tpgid = 1400;
            let _ = w.tick(TICK_MS);
            w.exit(1400, false);
        }
        w.close_block(1, 3);
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b13").map(|r| (r.verdict, r.exit)),
            Some((Verdict::Exited, Some(3))),
            "{seen}: {:?}",
            v.rows
        );
    }
}

/// A COMMAND LIST'S `D` THAT IS ITS `fg`'S STOP IS NO STATUS OF ITS OWN:
/// `make && fg` resumes vim, which is Ctrl-Z'd before a pass gave it the
/// block, so `fg` and the list return the stop's status. make's job takes
/// none, whether a pass saw the shell hold the terminal before the reader fed
/// that `D` or a `fg` typed at once put vim in front again before the pass,
/// its `C` read or not.
#[test]
fn a_command_lists_d_that_is_its_fgs_stop_is_no_status_of_its_own() {
    for case in ["between", "again", "again read"] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.p(1200).state = ProcState::Stopped;
        w.stop_block(1);
        let _ = w.tick(TICK_MS);
        w.run_fg(1, 13, "make && fg", sleeper(1300, "make"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.exit(1300, false);
        w.p(1200).state = ProcState::Sleeping;
        w.tab(1).tpgid = 1200;
        let _ = w.tick(TICK_MS);
        w.p(1200).state = ProcState::Stopped;
        if case == "between" {
            w.tab(1).tpgid = shell;
            let _ = w.tick(TICK_MS);
        }
        w.stop_block(1);
        if case.starts_with("again") {
            if case == "again read" {
                w.run_fg(1, 14, "fg", sleeper(1201, "unused"));
                w.exit(1201, false);
            }
            w.p(1200).state = ProcState::Sleeping;
            w.tab(1).tpgid = 1200;
        }
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b13").map(|r| (r.verdict, r.exit)),
            Some((Verdict::Exited, None)),
            "{case}: {:?}",
            v.rows
        );
    }
}

/// A LIST'S LATER COMMAND IN FRONT TAKES ITS BLOCK BACK: in bash's `make; fg
/// %1; make install`, the background `./fetch.sh` the list brought forward
/// ends within a pass and make install runs on, so the list's status is make
/// install's however long it ran; in `make; fg %1; fg %2`, the second job
/// `fg` resumed takes the block and its own status.
#[test]
fn a_lists_later_command_in_front_takes_its_block_back() {
    for second in [false, true] {
        let mut w = World::new();
        w.run_bg(1, sleeper(1250, "fetch.sh"));
        w.run_bg(1, sleeper(1260, "make"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        let command = if second {
            "make; fg %1; fg %2"
        } else {
            "make; fg %1; make install"
        };
        w.run_fg(1, 13, command, sleeper(1300, "make"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.exit(1300, false);
        w.tab(1).tpgid = 1250;
        let _ = w.tick(TICK_MS);
        w.exit(1250, false);
        if second {
            w.tab(1).tpgid = 1260;
            let _ = w.tick(TICK_MS);
            let _ = w.tick(TICK_MS);
            w.exit(1260, false);
            w.close_block(1, 2);
            let v = w.tick(TICK_MS);
            assert_eq!(
                v.row("j1260").map(|r| (r.verdict, r.exit)),
                Some((Verdict::Exited, Some(2))),
                "{:?}",
                v.rows
            );
            continue;
        }
        w.run_bg(1, sleeper(1400, "make"));
        w.tab(1).tpgid = 1400;
        let _ = run_to(&mut w, 20_000, |_, _| {});
        w.exit(1400, false);
        w.close_block(1, 0);
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b13").map(|r| (r.verdict, r.exit)),
            Some((Verdict::Exited, Some(0))),
            "{:?}",
            v.rows
        );
    }
}

/// A BLOCK WHOSE GROUP IN FRONT EMPTIED TAKES ITS LINES: in `seq 1 40 |
/// timeout 3600 xargs ./step.sh`, seq exits and timeout has left for a group
/// of its own, so the terminal's group holds no process while the shell waits
/// and the steps print; the block's job and the tab's one background group
/// that can run, timeout's, are judged by those lines, also beside a parked
/// editor.
#[test]
fn a_block_whose_group_in_front_emptied_takes_its_lines() {
    for (zombie, parked) in [(false, false), (true, false), (false, true)] {
        let mut w = World::new();
        if parked {
            w.run_fg(1, 2, "vim notes", sleeper(1200, "vim"));
            let _ = run_to(&mut w, 4_000, |_, _| {});
            w.p(1200).state = ProcState::Stopped;
            w.stop_block(1);
            let _ = w.tick(TICK_MS);
        }
        w.run_fg(
            1,
            3,
            "seq 1 40 | timeout 3600 xargs -n1 ./step.sh",
            sleeper(1300, "seq"),
        );
        w.run_bg(1, sleeper(1301, "timeout"));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.exit(1300, zombie);
        let v = run_to(&mut w, 10_000, |w, _| w.line(1));
        assert_eq!(
            (
                v.row("b3").map(|r| r.class),
                v.row("j1301").map(|r| r.class)
            ),
            (Some(Class::Running), Some(Class::Running)),
            "{zombie} {parked}: {:?}",
            v.rows
        );
    }
}

/// A BACKGROUND JOB TAKES NO LINES OF A PROGRAM IN FRONT WITHOUT A BLOCK: in a
/// shell that writes no OSC 133 marks, a silent `make &` beside the vim in
/// front is judged without the lines vim prints.
#[test]
fn a_background_job_takes_no_lines_of_a_program_in_front_without_a_block() {
    let mut w = World::new();
    w.run_bg(1, sleeper(1200, "vim"));
    w.run_bg(1, sleeper(1300, "make"));
    w.tab(1).tpgid = 1200;
    let v = run_to(&mut w, 10_000, |w, _| w.line(1));
    assert_eq!(
        (
            v.row("j1200").map(|r| r.class),
            v.row("j1300").map(|r| r.class == Class::Running)
        ),
        (Some(Class::Running), Some(false)),
        "{:?}",
        v.rows
    );
}

/// A PARKED JOB TAKES NO LINES: with vim Ctrl-Z'd and `ping -i 5 host &` the
/// one background group that runs, the lines at the prompt are ping's, and
/// vim stays parked.
#[test]
fn a_parked_job_takes_no_lines() {
    let mut w = World::new();
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_bg(1, sleeper(1300, "ping"));
    let v = run_to(&mut w, 10_000, |w, _| w.line(1));
    assert_eq!(
        (
            v.row("b12").map(|r| r.class),
            v.row("j1300").map(|r| r.class)
        ),
        (Some(Class::Stopped), Some(Class::Running)),
        "{:?}",
        v.rows
    );
}

/// A JOB TAKES ONLY LINES ONE OF ITS PROCESSES COULD HAVE WRITTEN: a job none
/// of whose processes made a system call over the tick is credited none of
/// the terminal's lines. A silent `python3 -m http.server &` takes none of
/// what `seq 1 40 | setsid xargs ./step.sh` prints once seq exits; a hung
/// `./sync.sh &` takes none of the line editor's redraws at the prompt; a
/// `git push` hung on its peer takes none of a `tail -f &`'s lines; and
/// `git diff | timeout 1800 claude -p`'s claude, the one background group,
/// takes its own lines while one of its tool calls runs.
#[test]
fn a_job_takes_only_lines_one_of_its_processes_could_have_written() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_bg(1, sleeper(1301, "python3"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.run_fg(
        1,
        3,
        "seq 1 40 | setsid xargs -n1 ./step.sh",
        sleeper(1300, "seq"),
    );
    let mut x = Proc::new(1302, shell, "xargs");
    x.sid = 1302;
    w.add(x);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1302]));
    assert_ne!(
        v.row("j1301").map(|r| r.class),
        Some(Class::Running),
        "{:?}",
        v.rows
    );

    let mut w = World::new();
    let mut sync = sleeper(1610, "sync.sh");
    sync.fds.push(tcp_fd(5));
    w.run_bg(1, sync);
    let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[shell]));
    assert_ne!(
        v.row("j1610").map(|r| r.class),
        Some(Class::Running),
        "{:?}",
        v.rows
    );

    let mut w = World::new();
    w.run_bg(1, sleeper(1600, "tail"));
    w.run_fg(1, 4, "git push", sleeper(1700, "git"));
    let mut remote = Proc::new(1701, 1700, "git-remote-https");
    remote.pgid = 1700;
    remote.sid = shell;
    remote.tty = true;
    remote.fds.push(tcp_fd(5));
    w.add(remote);
    let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1600]));
    assert_ne!(
        v.row("b4").map(|r| r.class),
        Some(Class::Running),
        "{:?}",
        v.rows
    );

    // A stage that printed its error and exited (`curl -sS URL | jq . &`) wrote the line.
    let mut w = World::new();
    w.run_bg(1, sleeper(1250, "curl"));
    let mut jq = Proc::new(1251, shell, "jq");
    jq.pgid = 1250;
    jq.sid = shell;
    jq.tty = true;
    w.add(jq);
    let _ = run_to(&mut w, 10_000, |_, _| {});
    w.advance(TICK_MS);
    w.line_from(1, &[1250]);
    w.exit(1250, false);
    let v = w.observe();
    assert_eq!(
        v.row("j1250").map(|r| r.class),
        Some(Class::Running),
        "{:?}",
        v.rows
    );

    let mut w = World::new();
    w.run_fg(
        1,
        9,
        "git diff main | timeout 1800 claude -p",
        sleeper(1500, "git"),
    );
    w.run_bg(1, sleeper(1501, "timeout"));
    let mut claude = Proc::new(1502, 1501, "claude");
    claude.pgid = 1501;
    claude.sid = shell;
    claude.tty = true;
    w.add(claude);
    let mut call = Proc::new(1600, 1502, "zsh");
    call.sid = 1600;
    w.add(call);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1500, false);
    let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1502]));
    assert_eq!(
        v.row("j1501").map(|r| r.class),
        Some(Class::Running),
        "{:?}",
        v.rows
    );
}

/// A COMMAND WHOSE OUTPUT A SERVER OUTSIDE THE TAB WRITES TAKES ITS LINES:
/// `ssh build make` over a shared connection hands its terminal to the master
/// ssh, outside the tab's tree, and waits on it over a socket (as tmux's
/// client does on its server); while no other job of the tab stirs (a
/// Ctrl-Z'd `sudo vim`, whose stopped processes aterm may not read, makes no
/// system call), the lines the tab prints are the command's in front. Hung
/// beside a `tail -f &` started with it, it takes none of tail's lines; a
/// command hung with no such socket takes none of a stray writer's (a server
/// a subshell left behind, no job's); one aterm may not read (`sudo mount`)
/// takes none of a background `ping`'s, read or not, nor the last line of a
/// `make &` that ended, nor a background job's that turned root's (`exec
/// sudo`); a hung `git fetch` takes none of the notice zsh
/// prints as it reaps `make &`; and a polling loop none of `rails server &`'s
/// log.
#[test]
fn a_command_whose_output_a_server_outside_the_tab_writes_takes_its_lines() {
    for parked in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        if parked {
            w.run_fg(1, 8, "sudo vim /etc/hosts", sleeper(1200, "sudo"));
            let mut vim = Proc::new(1201, 1200, "vim");
            vim.pgid = 1200;
            vim.sid = shell;
            vim.tty = true;
            w.add(vim);
            let _ = run_to(&mut w, 4_000, |_, _| {});
            for p in [1200, 1201] {
                w.p(p).state = ProcState::Stopped;
                w.p(p).refused = true;
            }
            w.stop_block(1);
            let _ = w.tick(TICK_MS);
        }
        let mut ssh = sleeper(1300, "ssh");
        ssh.fds.push(socket_fd(3));
        w.run_fg(1, 9, "ssh build make -j8", ssh);
        w.tty_mode(1, true, true);
        let v = run_to(&mut w, 1_200_000, |w, _| w.line_from(1, &[]));
        assert_eq!(
            v.row("b9").map(|r| (r.class, r.verdict)),
            Some((Class::Running, Verdict::Fine)),
            "{parked}: {:?}",
            v.rows
        );
    }

    // Hung beside a `tail -f log &` started on its line: tail's lines are tail's, also one the
    // reader fed a pass after tail wrote it.
    let mut w = World::new();
    let mut ssh = sleeper(1300, "ssh");
    ssh.fds.push(socket_fd(3));
    w.run_fg(1, 9, "tail -f log & ssh build make -j8", ssh);
    w.run_bg(1, sleeper(1400, "tail"));
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| match rel % 60_000 {
        0 => w.line_from(1, &[1400]),
        30_000 => *w.p(1400).counters.syscalls.get_or_insert(0) += 1,
        32_000 => w.line_from(1, &[]),
        _ => {}
    });
    assert_eq!(
        v.row("b9").map(|r| (r.verdict, r.quiet_ms)),
        Some((Verdict::Quiet, Some(1_200_000))),
        "{}",
        wire(&v, "b9")
    );

    // Hung on a lock, with no socket: what a server no job holds prints is none of its.
    let mut w = World::new();
    w.run_fg(1, 9, "python3 migrate.py", sleeper(1300, "python3"));
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel % 10_000 == 0 {
            w.line_from(1, &[]);
        }
    });
    assert_ne!(
        v.row("b9").map(|r| r.class),
        Some(Class::Running),
        "{}",
        wire(&v, "b9")
    );

    // `sudo rsync` stopped in front (`kill -STOP` from another tab), its process root's, while
    // a server no job holds prints: a stopped process wrote none of it.
    let mut w = World::new();
    w.run_fg(1, 10, "sudo rsync -a src dst", sleeper(1300, "sudo"));
    w.tty_mode(1, true, true);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1300).state = ProcState::Stopped;
    w.p(1300).refused = true;
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel % 10_000 == 0 {
            w.line_from(1, &[]);
        }
    });
    assert_ne!(
        v.row("b10").map(|r| r.class),
        Some(Class::Running),
        "{}",
        wire(&v, "b10")
    );

    // `sudo mount` hung, its processes root's, beside `ping -i 5 host &`, which prints: also a
    // `sudo ping` aterm may not read either.
    for refused in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        w.run_bg(1, sleeper(1400, "ping"));
        w.p(1400).refused = refused;
        w.run_fg(
            1,
            12,
            "sudo mount -t nfs srv:/x /mnt",
            sleeper(1300, "sudo"),
        );
        let mut mount = Proc::new(1301, 1300, "mount");
        mount.pgid = 1300;
        mount.sid = shell;
        mount.tty = true;
        w.add(mount);
        for p in [1300, 1301] {
            w.p(p).refused = true;
        }
        w.tty_mode(1, true, true);
        let v = run_to(&mut w, 1_200_000, |w, rel| {
            if rel % 6_000 == 0 {
                w.line_from(1, &[1400]);
            }
        });
        assert_ne!(
            v.row("b12").map(|r| r.class),
            Some(Class::Running),
            "{refused}: {}",
            wire(&v, "b12")
        );
    }

    // Hung beside `make &`, which prints until it ends: its last line, read in the pass that
    // sees it gone (reaped, or a zombie its shell has yet to reap), is make's, also when a pass
    // judges make first.
    for zombie in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        w.run_bg(1, sleeper(1400, "make"));
        let _ = w.observe();
        w.run_fg(
            1,
            12,
            "sudo mount -t nfs srv:/x /mnt",
            sleeper(1300, "sudo"),
        );
        let mut mount = Proc::new(1301, 1300, "mount");
        mount.pgid = 1300;
        mount.sid = shell;
        mount.tty = true;
        w.add(mount);
        for p in [1300, 1301] {
            w.p(p).refused = true;
        }
        w.tty_mode(1, true, true);
        let v = run_to(&mut w, 1_200_000, |w, rel| {
            if rel <= 600_000 && rel % 6_000 == 0 {
                w.line_from(1, &[1400]);
            }
            if rel == 600_000 {
                w.exit(1400, zombie);
            }
        });
        assert_eq!(
            v.row("b12").map(|r| r.quiet_ms),
            Some(Some(1_200_000)),
            "{zombie}: {}",
            wire(&v, "b12")
        );
    }

    // Hung beside `(sleep 600; exec sudo tcpdump) &`, whose subshell turns root's as it runs
    // tcpdump in its place and prints: none of tcpdump's lines, its first included.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_bg(1, sleeper(1400, "zsh"));
    w.run_fg(
        1,
        12,
        "sudo mount -t nfs srv:/x /mnt",
        sleeper(1300, "sudo"),
    );
    let mut mount = Proc::new(1301, 1300, "mount");
    mount.pgid = 1300;
    mount.sid = shell;
    mount.tty = true;
    w.add(mount);
    for p in [1300, 1301] {
        w.p(p).refused = true;
    }
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel == 600_000 {
            let p = w.p(1400);
            p.name = "sudo".into();
            p.exec += 1;
            p.refused = true;
        }
        if rel >= 600_000 && rel % 6_000 == 0 {
            w.line_from(1, &[1400]);
        }
    });
    assert_eq!(
        v.row("b12").map(|r| r.quiet_ms),
        Some(Some(1_200_000)),
        "{}",
        wire(&v, "b12")
    );

    // `git fetch` hung on its peer while `make &` ends: zsh wakes to reap make and prints its
    // notice, none of the fetch's work.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_bg(1, sleeper(1400, "make"));
    let mut git = sleeper(1300, "git");
    git.fds.push(tcp_fd(5));
    w.run_fg(1, 9, "git fetch", git);
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel < 900_000 && rel % 6_000 == 0 {
            w.line_from(1, &[1400]);
        }
        if rel == 900_000 {
            w.exit(1400, false);
            w.line_from(1, &[shell]);
        }
    });
    assert_eq!(
        v.row("b9").map(|r| r.quiet_ms),
        Some(Some(1_200_000)),
        "{}",
        wire(&v, "b9")
    );

    // `until pg_isready; do sleep 1; done` beside `rails server &`, which logs: the loop's
    // short commands come and go, the shell forking and reaping each, and rails writes the lines.
    let mut w = World::new();
    w.run_bg(1, sleeper(1400, "rails"));
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 9,
        started_ms: at,
        open: true,
        command: "until pg_isready; do sleep 1; done".to_owned(),
        status: None,
        stopped: false,
    });
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        let born = 3_000 + u32::try_from(rel / TICK_MS).unwrap_or(0);
        w.exit(born - 1, false);
        let mut p = Proc::new(
            born,
            shell,
            if born % 2 == 0 { "pg_isready" } else { "sleep" },
        );
        p.sid = shell;
        p.tty = true;
        w.add(p);
        w.tab(1).tpgid = born;
        *w.p(shell).counters.syscalls.get_or_insert(0) += 3;
        if rel % 4_000 == 0 {
            w.line_from(1, &[1400]);
        }
    });
    assert_ne!(
        v.row("b9").map(|r| r.class),
        Some(Class::Running),
        "{}",
        wire(&v, "b9")
    );
}

/// A GROUP A PROGRAM MADE FOR ITS CHILDREN IS ITS JOB'S: ninja starts each
/// step in a group of its own and prints only its `\r` status line; the steps
/// are the build's members, whose work is its progress, and no step is a row
/// of its own. A stop in such a group is held (no `fg` resumes it), also under
/// a root that never put a group in front; a job stopped in the group job
/// control put in front too parks by that stop.
#[test]
fn a_group_a_program_made_for_its_children_is_its_jobs() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_fg(1, 7, "ninja -C build", sleeper(1300, "ninja"));
    w.tty_mode(1, true, true);
    let mut step = 0u32;
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel % 4_000 != 0 {
            return;
        }
        let sh = 2000 + step * 2;
        if step > 0 {
            // ninja reaps the step: its CPU is ninja's children's.
            let done = w.p(sh - 2).counters.cpu_ns + w.p(sh - 1).counters.cpu_ns;
            let reaped = &mut w.p(1300).counters.child_cpu_ns;
            *reaped = Some(reaped.unwrap_or(0) + done);
            w.exit(sh - 2, false);
            w.exit(sh - 1, false);
        }
        let mut p = Proc::new(sh, 1300, "sh");
        p.sid = shell;
        p.tty = true;
        w.add(p);
        let mut cc = spinner(sh + 1, "clang");
        cc.ppid = sh;
        cc.pgid = sh;
        cc.sid = shell;
        cc.tty = true;
        w.add(cc);
        step += 1;
    });
    assert!(
        v.rows.iter().all(|r| r.id == "b7"),
        "{:?}",
        v.rows.iter().map(|r| r.wire()).collect::<Vec<_>>()
    );
    assert_eq!(
        v.row("b7").map(|r| r.verdict),
        Some(Verdict::Fine),
        "{}",
        wire(&v, "b7")
    );

    // `ninja -C build &` whose step stopped at an ssh passphrase prompt (its group ninja's, not
    // the shell's): held, as no `fg` resumes it, and the build reads stuck in time.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_bg(1, sleeper(1300, "ninja"));
    for (pid, ppid, name) in [(1310, 1300, "sh"), (1311, 1310, "ssh")] {
        let mut p = Proc::new(pid, ppid, name);
        p.pgid = 1310;
        p.sid = shell;
        p.tty = true;
        p.state = ProcState::Stopped;
        w.add(p);
    }
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("j1300").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{}",
        wire(&v, "j1300")
    );

    // The same step under `aterm -e ninja`: the root that made its group never put one in front,
    // so it does no job control either.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.p(shell).name = "ninja".into();
    for (pid, ppid, name) in [(2310, shell, "sh"), (2311, 2310, "ssh")] {
        let mut p = Proc::new(pid, ppid, name);
        p.pgid = 2310;
        p.sid = shell;
        p.tty = true;
        p.state = ProcState::Stopped;
        w.add(p);
    }
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("j2310").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{}",
        wire(&v, "j2310")
    );

    // Ctrl-Z of `cargo nextest run`, which starts each test in a group of its own and stops them
    // as it stops: the job parks by nextest's stop, in the group the shell put in front, whatever
    // pid a test drew below it.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.mark(1);
    w.run_fg(1, 7, "cargo nextest run", sleeper(1900, "cargo-nextest"));
    for pid in [1210, 1244] {
        let mut t = Proc::new(pid, 1900, "aterm_core-1a2b");
        t.sid = shell;
        t.tty = true;
        w.add(t);
    }
    let _ = run_to(&mut w, 10_000, |_, _| {});
    for pid in [1900, 1210, 1244] {
        w.p(pid).state = ProcState::Stopped;
    }
    w.stop_block(1);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("b7").map(|r| r.verdict),
        Some(Verdict::Parked),
        "{}",
        wire(&v, "b7")
    );
}

/// A NESTED SHELL'S JOBS ARE ROWS OF THEIR OWN: `bash` run in the tab's zsh
/// put `vim` in front, so it does job control, and its `curl … &` hung on a
/// silent server is a background job that reads stuck, never part of the
/// shell's row that reads the person's keys at its prompt; its `vim &`, which
/// stopped reading the terminal from the background, waits for bash's `fg`:
/// parked.
#[test]
fn a_nested_shells_jobs_are_rows_of_their_own() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_fg(1, 3, "bash", sleeper(1200, "bash"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    let mut vim = Proc::new(1210, 1200, "vim");
    vim.sid = shell;
    vim.tty = true;
    w.add(vim);
    w.tab(1).tpgid = 1210;
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1210, false);
    w.tab(1).tpgid = 1200;
    let mut get = curl(1220);
    get.ppid = 1200;
    get.sid = shell;
    get.tty = true;
    w.add(get);
    let mut edit = Proc::new(1230, 1200, "vim");
    edit.sid = shell;
    edit.tty = true;
    edit.state = ProcState::Stopped;
    w.add(edit);
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        (
            v.row("j1220").map(|r| r.verdict),
            v.row("j1230").map(|r| r.verdict),
            v.row("b3").map(|r| (r.verdict, r.members))
        ),
        (
            Some(Verdict::Stuck),
            Some(Verdict::Parked),
            Some((Verdict::Fine, 2))
        ),
        "{:?}",
        v.rows
    );
}

/// A TERMINAL SESSION NESTED IN THE TAB'S IS A ROW OF ITS OWN: sudo under
/// `use_pty` runs its command (and its monitor) in a session of its own on a
/// new terminal, as `script` runs its shell, and an agent its pty tool call.
/// That session is one row beside its opener's, judged by its own processes:
/// it takes the lines it writes while its opener holds the terminal (through
/// each session the opener sits in), and a command of it hung on a silent
/// peer reads stuck while sudo's row reads the terminal and the agent's row
/// runs on its redraws, also when the agent runs in `nvim`'s terminal. A
/// session in another is a row of its own, and one whose reader in front waits
/// for the person's keys reads its own terminal's word: fine (a shell in
/// Emacs's buffer at its prompt reads a line).
#[test]
fn a_terminal_session_nested_in_the_tabs_is_a_row_of_its_own() {
    // Root's processes: none read (macOS refuses another user's counters), or read without a
    // system call count (Linux's `io` is root's alone).
    for (script, hung, root) in [
        (false, false, 0),
        (true, false, 0),
        (true, false, 1),
        (false, false, 1),
        (false, false, 2),
        (false, false, 3),
        (false, true, 0),
    ] {
        let mut w = World::new();
        let command = if script {
            "script -q log sudo journalctl -f"
        } else {
            "sudo journalctl -f"
        };
        let opener = if script { "script" } else { "sudo" };
        w.run_fg(1, 5, command, sleeper(1300, opener));
        let mut sudo = 1300;
        if script {
            let mut inner = Proc::new(1310, 1300, "zsh");
            inner.sid = 1310;
            inner.tty = true;
            w.add(inner);
            let mut s = Proc::new(1311, 1310, "sudo");
            s.sid = 1310;
            s.tty = true;
            w.add(s);
            w.fronts.insert(1310, 1311);
            sudo = 1311;
        }
        let mut monitor = Proc::new(1301, sudo, "sudo");
        monitor.sid = 1301;
        monitor.tty = true;
        w.add(monitor);
        let mut cmd = Proc::new(1302, 1301, if hung { "mount" } else { "journalctl" });
        cmd.sid = 1301;
        cmd.tty = true;
        if hung {
            cmd.fds.push(tcp_fd(5));
        }
        w.add(cmd);
        for pid in [sudo, 1301, 1302] {
            match root {
                1 if pid != sudo => w.p(pid).refused = true,
                2 if pid != sudo => w.p(pid).counters.syscalls = None,
                // `sudo -u $USER`: sudo and its monitor root's, the command yours.
                3 if pid != 1302 => w.p(pid).refused = true,
                _ => {}
            }
        }
        w.tty_mode(1, false, false);
        let n = "n1301";
        let v = if hung {
            run_to(&mut w, 1_200_000, |_, _| {})
        } else {
            run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1300, 1302]))
        };
        assert!(
            v.rows.iter().all(|r| !r.id.starts_with('j')),
            "{script} {hung} {root}: {:?}",
            v.rows
        );
        let want = if hung {
            (Verdict::Fine, Verdict::Stuck)
        } else {
            (Verdict::Fine, Verdict::Fine)
        };
        assert_eq!(
            (
                v.row("b5").map(|r| (r.verdict, r.members)),
                v.row(n).map(|r| (r.verdict, r.members)),
                v.row("n1310").map(|r| (r.verdict, r.members))
            ),
            (
                Some((want.0, 2)),
                Some((want.1, 2)),
                script.then_some((Verdict::Fine, 2))
            ),
            "{script} {hung} {root}: {:?}",
            v.rows
        );
        if !hung {
            assert_eq!(
                (v.row("b5").map(|r| r.class), v.row(n).map(|r| r.class)),
                (Some(Class::Running), Some(Class::Running)),
                "{script} {root}: {:?}",
                v.rows
            );
        }
    }

    let mut w = World::new();
    w.run_fg(1, 5, "codex", sleeper(1300, "codex"));
    let mut call = Proc::new(1301, 1300, "bash");
    call.sid = 1301;
    call.tty = true;
    w.add(call);
    let mut test = Proc::new(1302, 1301, "node");
    test.sid = 1301;
    test.tty = true;
    test.fds.push(tcp_fd(5));
    w.add(test);
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 1_200_000, |w, _| w.line_from(1, &[1300]));
    assert_eq!(
        (
            v.row("b5").map(|r| r.class),
            v.row("n1301").map(|r| r.verdict)
        ),
        (Some(Class::Running), Some(Verdict::Stuck)),
        "{:?}",
        v.rows
    );

    // An agent in `nvim`'s terminal: its redraws are that terminal's session's, and its pty
    // call hung on a silent peer reads stuck.
    let mut w = World::new();
    w.run_fg(1, 5, "nvim", sleeper(1300, "nvim"));
    let mut term = Proc::new(1310, 1300, "zsh");
    term.sid = 1310;
    term.tty = true;
    w.add(term);
    let mut codex = Proc::new(1311, 1310, "codex");
    codex.sid = 1310;
    codex.tty = true;
    w.add(codex);
    w.fronts.insert(1310, 1311);
    let mut call = Proc::new(1320, 1311, "bash");
    call.sid = 1320;
    call.tty = true;
    w.add(call);
    let mut test = Proc::new(1321, 1320, "node");
    test.sid = 1320;
    test.tty = true;
    test.fds.push(tcp_fd(5));
    w.add(test);
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 1_200_000, |w, _| w.line_from(1, &[1311]));
    assert_eq!(
        (
            v.row("n1310").map(|r| r.class),
            v.row("n1320").map(|r| r.verdict)
        ),
        (Some(Class::Running), Some(Verdict::Stuck)),
        "{:?}",
        v.rows
    );

    // `vim` in `script`'s shell, the person reading what it shows: its terminal is raw, and its
    // plugins' timers are no polling loop.
    let mut w = World::new();
    w.run_fg(1, 5, "script -q log", sleeper(1300, "script"));
    let mut inner = Proc::new(1310, 1300, "zsh");
    inner.sid = 1310;
    inner.tty = true;
    w.add(inner);
    w.fronts.insert(1310, 1310);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    typed_into(&mut w, 1310);
    // A key stamped as a pass ends, which zsh reads in the next: one still tick unlatches nothing,
    // and zsh's line editor at its prompt reads raw keys.
    let _ = w.tick(TICK_MS);
    w.advance(TICK_MS);
    w.typed(1, 1300);
    let v = w.observe();
    assert_eq!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyRaw),
        "{}",
        wire(&v, "n1310")
    );
    let mut vim = Proc::new(1311, 1310, "vim");
    vim.sid = 1310;
    vim.tty = true;
    vim.syscalls_per_s = 50;
    w.add(vim);
    w.fronts.insert(1310, 1311);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("n1310")
            .map(|r| (r.verdict, r.wait.as_ref().map(|w| w.name))),
        Some((Verdict::Fine, Some(WaitName::TtyRaw))),
        "{:?}",
        v.rows
    );
    // Frozen, the keys typed since queue in its terminal: no reader takes them.
    w.terms.get_mut(&1310).expect("its word").queued = 20;
    let v = w.tick(TICK_MS);
    assert_ne!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyRaw),
        "{}",
        wire(&v, "n1310")
    );

    // `ssh` at its passphrase prompt in `script`'s shell, the person typing: a line reader takes
    // the keys at Enter, so its still ticks unlatch nothing.
    let mut w = World::new();
    w.run_fg(1, 5, "script -q log", sleeper(1300, "script"));
    let mut inner = Proc::new(1310, 1300, "zsh");
    inner.sid = 1310;
    inner.tty = true;
    w.add(inner);
    w.fronts.insert(1310, 1310);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    typed_into(&mut w, 1310);
    let mut ssh = Proc::new(1311, 1310, "ssh");
    ssh.sid = 1310;
    ssh.tty = true;
    w.add(ssh);
    w.fronts.insert(1310, 1311);
    w.terms.insert(
        1310,
        TermWord {
            canonical: true,
            echo: false,
            queued: 0,
        },
    );
    let v = run_to(&mut w, 120_000, |w, _| w.typed(1, 1300));
    assert_eq!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyPassword),
        "{}",
        wire(&v, "n1310")
    );

    // A build in front of `script`'s shell prints while `script` stops reading the person's keys
    // (the tab's gate published): the build's row is judged by its lines, the stall `script`'s.
    let mut w = World::new();
    w.run_fg(1, 5, "script -q log", sleeper(1300, "script"));
    let mut inner = Proc::new(1310, 1300, "zsh");
    inner.sid = 1310;
    inner.tty = true;
    w.add(inner);
    w.fronts.insert(1310, 1310);
    typed_into(&mut w, 1310);
    let mut make = Proc::new(1311, 1310, "make");
    make.sid = 1310;
    make.tty = true;
    w.add(make);
    w.fronts.insert(1310, 1311);
    w.gate(1, GateWord::Stalled, 20, 12_000);
    let v = run_to(&mut w, 30_000, |w, _| w.line_from(1, &[1300, 1311]));
    assert_eq!(
        v.row("n1310").map(|r| r.class),
        Some(Class::Running),
        "{}",
        wire(&v, "n1310")
    );

    // Codex's pty call at `create-vite`'s raw prompt (after `npm install` polled while the person
    // typed to Codex) and Emacs's shell at a hung `curl`: the person's keys reach Codex's own
    // prompt, never the call's terminal, which wakes only to the agent's keys, and Emacs keeps
    // echo off in every shell it opens; each reads by its leaves, not as the person's read.
    for emacs in [false, true] {
        let mut w = World::new();
        let opener = if emacs { "emacs" } else { "codex" };
        w.run_fg(1, 5, opener, sleeper(1300, opener));
        let mut lead = Proc::new(1310, 1300, "bash");
        lead.sid = 1310;
        lead.tty = true;
        w.add(lead);
        w.fronts.insert(1310, 1310);
        w.terms.insert(
            1310,
            TermWord {
                canonical: emacs,
                echo: false,
                queued: 0,
            },
        );
        w.tty_mode(1, false, false);
        if emacs {
            typed_into(&mut w, 1310);
        }
        let mut leaf = if emacs {
            curl(1311)
        } else {
            // `npm install` polls before `create-vite`'s raw prompt waits for an arrow key.
            let mut p = Proc::new(1311, 1310, "node");
            p.syscalls_per_s = 50;
            p
        };
        leaf.ppid = 1310;
        leaf.sid = 1310;
        leaf.tty = true;
        w.add(leaf);
        w.fronts.insert(1310, 1311);
        // The person types to Codex from the call's first interval on.
        let v = run_to(&mut w, 1_200_000, |w, rel| {
            if rel % 20_000 == 0 || rel == TICK_MS {
                w.typed(1, 1300);
            }
            if rel == 60_000 {
                w.p(1311).syscalls_per_s = 0;
            }
            // The agent writes the call keys of its own as the person types to it, twice, the
            // person's keys to Codex between.
            if (rel == 80_000 || rel == 200_000) && !emacs {
                *w.p(1311).counters.syscalls.get_or_insert(0) += 1;
            }
            // The call's shell wakes (a child's exit) over two passes you typed in: what sits in
            // front of its terminal took no key.
            if (rel == 140_000 || rel == 160_000) && !emacs {
                *w.p(1310).counters.syscalls.get_or_insert(0) += 1;
            }
        });
        let row = v.row("n1310").expect("the session's row");
        assert!(
            !matches!(row.verdict, Verdict::Fine | Verdict::WaitingYou)
                && row.wait.as_ref().is_none_or(|w| {
                    !matches!(w.name, WaitName::TtyRaw | WaitName::TtyPassword)
                }),
            "{emacs}: {}",
            row.wire()
        );
    }

    // Emacs's shell at its prompt, the lines the person sends it read: it reads a line, fine.
    let mut w = World::new();
    w.run_fg(1, 5, "emacs", sleeper(1300, "emacs"));
    let mut bash = Proc::new(1310, 1300, "bash");
    bash.sid = 1310;
    bash.tty = true;
    w.add(bash);
    w.fronts.insert(1310, 1310);
    w.terms.insert(
        1310,
        TermWord {
            canonical: true,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    typed_into(&mut w, 1310);
    let v = run_to(&mut w, 600_000, |_, _| {});
    assert_eq!(
        v.row("n1310")
            .map(|r| (r.verdict, r.wait.as_ref().map(|w| w.name))),
        Some((Verdict::Fine, Some(WaitName::TtyBuiltinRead))),
        "{}",
        wire(&v, "n1310")
    );
    // Frozen, the line sent since queues in its terminal: it reads none (by two agreeing passes).
    w.terms.get_mut(&1310).expect("its word").queued = 20;
    let _ = w.tick(TICK_MS);
    let v = w.tick(TICK_MS);
    assert_ne!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyBuiltinRead),
        "{}",
        wire(&v, "n1310")
    );

    // Emacs's `sh -c` (`C-u M-x compile`, no job control) runs a test that read the lines the
    // person sent and then hung in sh's own group, on its peer or on a lock with no descriptor to
    // name: sh reads no line, and the test reads stuck in time (quiet, with no wait to name).
    for socket in [true, false] {
        let mut w = World::new();
        w.run_fg(1, 5, "emacs", sleeper(1300, "emacs"));
        let mut sh = Proc::new(1310, 1300, "sh");
        sh.sid = 1310;
        sh.tty = true;
        w.add(sh);
        let mut test = Proc::new(1311, 1310, "python3");
        test.pgid = 1310;
        test.sid = 1310;
        test.tty = true;
        if socket {
            test.fds.push(tcp_fd(5));
        }
        w.add(test);
        w.fronts.insert(1310, 1310);
        w.terms.insert(
            1310,
            TermWord {
                canonical: true,
                echo: false,
                queued: 0,
            },
        );
        w.tty_mode(1, false, false);
        typed_into(&mut w, 1311);
        let v = run_to(&mut w, 1_200_000, |_, _| {});
        assert_eq!(
            v.row("n1310").map(|r| r.verdict),
            Some(if socket {
                Verdict::Stuck
            } else {
                Verdict::Quiet
            }),
            "{socket}: {}",
            wire(&v, "n1310")
        );
    }

    // The person types to Codex as its pty call starts polling and once more as the agent writes
    // into that call, then waits: two chance wakes, the first from no reading at all, show no
    // key of the person's reaches the call.
    let mut w = World::new();
    w.run_fg(1, 5, "codex", sleeper(1300, "codex"));
    let mut lead = Proc::new(1310, 1300, "bash");
    lead.sid = 1310;
    lead.tty = true;
    w.add(lead);
    let mut node = Proc::new(1311, 1310, "node");
    node.sid = 1310;
    node.tty = true;
    node.syscalls_per_s = 50;
    node.fds.push(tcp_fd(5));
    w.add(node);
    w.fronts.insert(1310, 1311);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel == TICK_MS || rel == 80_000 {
            w.typed(1, 1300);
        }
        if rel == 60_000 {
            w.p(1311).syscalls_per_s = 0;
        }
        if rel == 80_000 {
            *w.p(1311).counters.syscalls.get_or_insert(0) += 1;
        }
    });
    assert_eq!(
        v.row("n1310").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{}",
        wire(&v, "n1310")
    );

    // Codex opened the person's editor (its Ctrl-G) over its pty call: the keys typed there reach
    // the editor, also in the passes the call woke in, and none reach the call once Codex is back.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.run_fg(1, 5, "codex", sleeper(1300, "codex"));
    let mut lead = Proc::new(1310, 1300, "bash");
    lead.sid = 1310;
    lead.tty = true;
    w.add(lead);
    let mut node = Proc::new(1311, 1310, "node");
    node.sid = 1310;
    node.tty = true;
    node.fds.push(tcp_fd(5));
    w.add(node);
    w.fronts.insert(1310, 1311);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    let mut vim = Proc::new(1320, 1300, "vim");
    vim.sid = shell;
    vim.tty = true;
    w.add(vim);
    w.tab(1).tpgid = 1320;
    let _ = run_to(&mut w, 60_000, |w, rel| {
        if rel == 20_000 || rel == 40_000 {
            w.typed(1, 1320);
            *w.p(1311).counters.syscalls.get_or_insert(0) += 1;
        }
    });
    w.exit(1320, false);
    w.tab(1).tpgid = 1300;
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("n1310").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{}",
        wire(&v, "n1310")
    );

    // Its opener in the background (of the tab, or of `script`'s shell while `make` runs in
    // front there), the lines a command in front prints are none of a session that only polls.
    for inner in [false, true] {
        let mut w = World::new();
        let opener = if inner {
            w.run_fg(1, 6, "script -q log", sleeper(1300, "script"));
            let mut sh = Proc::new(1310, 1300, "zsh");
            sh.sid = 1310;
            sh.tty = true;
            w.add(sh);
            let mut sudo = Proc::new(1311, 1310, "sudo");
            sudo.sid = 1310;
            sudo.tty = true;
            w.add(sudo);
            let mut make = Proc::new(1400, 1310, "make");
            make.sid = 1310;
            make.tty = true;
            w.add(make);
            w.fronts.insert(1310, 1400);
            1311
        } else {
            w.run_bg(1, sleeper(1300, "sudo"));
            w.run_fg(1, 6, "make", sleeper(1400, "make"));
            1300
        };
        let mut monitor = Proc::new(1301, opener, "sudo");
        monitor.sid = 1301;
        monitor.tty = true;
        w.add(monitor);
        let mut poll = Proc::new(1302, 1301, "poller");
        poll.sid = 1301;
        poll.tty = true;
        poll.syscalls_per_s = 50;
        w.add(poll);
        let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1400]));
        assert_ne!(
            v.row("n1301").map(|r| r.class),
            Some(Class::Running),
            "{inner}: {:?}",
            v.rows
        );
    }

    // Its leader gone, what it left (a `nohup` job of `script`'s shell, which launchd took in)
    // ends its row.
    let mut w = World::new();
    w.run_fg(1, 7, "script -q log", sleeper(1300, "script"));
    let mut inner = Proc::new(1310, 1300, "zsh");
    inner.sid = 1310;
    inner.tty = true;
    w.add(inner);
    let mut left = Proc::new(1311, 1310, "server");
    left.sid = 1310;
    left.tty = true;
    w.add(left);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1310, false);
    w.p(1311).ppid = 1;
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("n1310").map(|r| r.verdict),
        Some(Verdict::Exited),
        "{:?}",
        v.rows
    );
}

/// THE SHELL'S OWN PTY IS NO COMMAND'S: an async prompt's worker (pure's and
/// spaceship's zsh-async, run in a `zpty` the shell opened, a fork that kept
/// the shell's program) and the `git` it runs belong to no command's row and
/// leave none behind, also in a zsh run in the tab's and one swept before the
/// shell's first mark. A pty a program other than the shell opened is a
/// session of its own, also one a fork that kept that program leads (Python's
/// `pty.fork()`), the tab's root an agent.
#[test]
fn the_shells_own_pty_is_no_commands() {
    // One swept before the shell's first mark: no row.
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    let mut worker = Proc::new(1210, shell, "zsh");
    worker.sid = 1210;
    worker.tty = true;
    worker.forked = true;
    w.add(worker);
    let v = run_to(&mut w, 4_000, |_, _| {});
    assert!(v.rows.is_empty(), "{:?}", v.rows);

    let mut w = World::new();
    w.mark(1);
    let _ = w.observe();

    // A zsh run in the tab's zsh opens its own prompt's worker: still no command's.
    w.run_fg(1, 3, "zsh", sleeper(1200, "zsh"));
    let mut worker = Proc::new(1220, 1200, "zsh");
    worker.sid = 1220;
    worker.tty = true;
    worker.forked = true;
    w.add(worker);
    let v = run_to(&mut w, 4_000, |_, _| {});
    assert_eq!(
        (v.row("b3").map(|r| r.members), v.row("n1220").is_none()),
        (Some(2), true),
        "{:?}",
        v.rows
    );

    // `python3 tool.py`, whose `pty.fork()` child kept python's program, hung on a silent peer:
    // a session of the command's, which reads stuck.
    let mut w = World::new();
    w.mark(1);
    w.run_fg(1, 4, "python3 tool.py", sleeper(1200, "python3"));
    let mut child = Proc::new(1210, 1200, "python3");
    child.sid = 1210;
    child.tty = true;
    child.forked = true;
    child.fds.push(tcp_fd(5));
    w.add(child);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("n1210").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{:?}",
        v.rows
    );

    // A tab whose shell gave way to an agent (`exec codex`, after a prompt) or whose root is one
    // (`aterm -e codex`): each of the agent's pty calls is a session of its own, one running
    // `codex exec` too, and hung on a silent peer each reads stuck.
    for exec in [true, false] {
        let mut w = World::new();
        if exec {
            w.mark(1);
            let _ = w.observe();
            w.tab(1).tab.block = Some(Block {
                id: 4,
                started_ms: w.at,
                open: true,
                command: "exec codex".into(),
                status: None,
                stopped: false,
            });
        }
        w.p(shell).name = "codex".into();
        for (pid, name) in [(1210, "bash"), (1220, "codex")] {
            let mut call = Proc::new(pid, shell, name);
            call.sid = pid;
            call.tty = true;
            w.add(call);
            let mut test = Proc::new(pid + 1, pid, "node");
            test.sid = pid;
            test.tty = true;
            test.fds.push(tcp_fd(5));
            w.add(test);
        }
        let v = run_to(&mut w, 1_200_000, |_, _| {});
        assert_eq!(
            (
                v.row("n1210").map(|r| r.verdict),
                v.row("n1220").map(|r| r.verdict)
            ),
            (Some(Verdict::Stuck), Some(Verdict::Stuck)),
            "{exec}: {:?}",
            v.rows
        );
    }
}

/// A STOPPED SUDO PARKS WITH ITS COMMAND: Ctrl-Z of `sudo vim` stops vim, its
/// monitor stays awake and takes its terminal back, and sudo stops itself;
/// both rows read parked, and `ping -i 5 host &`, the one background group
/// that can print, takes its lines. A job the shell in `script` parked is
/// that shell's: the session reads by what holds its terminal, the shell at
/// its prompt (fine) or a command hung in front (stuck).
#[test]
fn a_stopped_sudo_parks_with_its_command() {
    let mut w = World::new();
    w.run_fg(1, 12, "sudo vim /etc/hosts", sleeper(1300, "sudo"));
    let mut monitor = Proc::new(1301, 1300, "sudo");
    monitor.sid = 1301;
    monitor.tty = true;
    w.add(monitor);
    let mut vim = Proc::new(1302, 1301, "vim");
    vim.sid = 1301;
    vim.tty = true;
    w.add(vim);
    w.fronts.insert(1301, 1302);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1302).state = ProcState::Stopped;
    w.p(1300).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_bg(1, sleeper(1400, "ping"));
    let v = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1400]));
    assert_eq!(
        (
            v.row("b12").map(|r| r.verdict),
            v.row("n1301").map(|r| r.verdict),
            v.row("j1400").map(|r| r.class)
        ),
        (
            Some(Verdict::Parked),
            Some(Verdict::Parked),
            Some(Class::Running)
        ),
        "{:?}",
        v.rows
    );

    // Ctrl-Z in the shell `script` runs: its shell takes its own terminal back (raw, at its
    // line editor), `script` holds the tab's; then a download hangs in front of that terminal.
    for hung in [false, true] {
        let mut w = World::new();
        w.run_fg(1, 13, "script -q log", sleeper(1300, "script"));
        let mut inner = Proc::new(1310, 1300, "zsh");
        inner.sid = 1310;
        inner.tty = true;
        w.add(inner);
        w.fronts.insert(1310, 1310);
        typed_into(&mut w, 1310);
        let mut vim = Proc::new(1311, 1310, "vim");
        vim.sid = 1310;
        vim.tty = true;
        w.add(vim);
        w.fronts.insert(1310, 1311);
        w.terms.insert(
            1310,
            TermWord {
                canonical: false,
                echo: false,
                queued: 0,
            },
        );
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.p(1311).state = ProcState::Stopped;
        w.fronts.insert(1310, 1310);
        if hung {
            let mut get = curl(1312);
            get.ppid = 1310;
            get.sid = 1310;
            get.tty = true;
            w.add(get);
            w.fronts.insert(1310, 1312);
            w.terms.insert(
                1310,
                TermWord {
                    canonical: true,
                    echo: true,
                    queued: 0,
                },
            );
        }
        let v = run_to(&mut w, 1_200_000, |_, _| {});
        assert_eq!(
            v.row("n1310").map(|r| r.verdict),
            Some(if hung { Verdict::Stuck } else { Verdict::Fine }),
            "{hung}: {:?}",
            v.rows
        );
    }
}

/// A BACKGROUND JOB WHOSE LEADER LEFT ONLY AN ADOPTED CHILD IS OVER:
/// `./start-server.sh &` started `nohup ./server &` in its group and exited,
/// so launchd adopted the server; the job's row ends, and the server is
/// nobody's. A debugger that attached from another tab adopts nothing: the
/// job runs on.
#[test]
fn a_background_job_whose_leader_left_only_an_adopted_child_is_over() {
    for (traced, reaper) in [(false, false), (false, true), (true, false)] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        if reaper {
            // A Linux desktop's `systemd --user` above aterm takes in what the tab's jobs leave.
            w.add(Proc::new(50, 1, "systemd"));
            w.p(100).ppid = 50;
        }
        w.run_bg(1, sleeper(1250, "start-server.sh"));
        let mut server = Proc::new(1251, 1250, "server");
        server.pgid = 1250;
        server.sid = shell;
        server.tty = true;
        w.add(server);
        let _ = run_to(&mut w, 4_000, |_, _| {});
        if traced {
            // A debugger another tab runs attached to it: macOS makes the tracer its parent.
            w.shell_tab(2, 2, 300);
            w.add(Proc::new(310, 300, "debugserver"));
            w.p(1250).ppid = 310;
        } else {
            w.exit(1250, false);
            w.p(1251).ppid = if reaper { 50 } else { 1 };
        }
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("j1250").map(|r| r.verdict == Verdict::Exited),
            Some(!traced),
            "{traced} {reaper}: {:?}",
            v.rows
        );
    }
}

/// A COMMAND TYPED AHEAD OF A BLOCK'S END ENDS NOTHING BUT THAT BLOCK: zsh
/// runs the `less x` typed while make ran once it wrote make's `D`, and a
/// pass that reads that `D` with less in front, before less's own `C`, ends
/// make's job with make's status; less is its own job, which its block then
/// takes.
#[test]
fn a_command_typed_ahead_of_a_blocks_end_ends_nothing_but_that_block() {
    let mut w = World::new();
    w.run_fg(1, 7, "make", sleeper(1000, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.run_bg(1, sleeper(1001, "less"));
    w.tab(1).tpgid = 1001;
    let _ = w.tick(TICK_MS);
    w.close_block(1, 0);
    w.tab(1).tpgid = 1001;
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b7").map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, Some(0))),
        "{:?}",
        v.rows
    );
    w.run_fg(1, 8, "less x", sleeper(1003, "unused"));
    w.exit(1003, false);
    w.tab(1).tpgid = 1001;
    let v = run_to(&mut w, 4_000, |_, _| {});
    let less: Vec<_> = v
        .rows
        .iter()
        .filter(|r| r.verdict != Verdict::Exited && r.exe == "less")
        .collect();
    assert_eq!(less.len(), 1, "{:?}", v.rows);
    assert_ne!(less[0].id, "b7", "{:?}", v.rows);
    assert_eq!(less[0].kind, JobKind::Shell, "{:?}", v.rows);
}

/// TWO JOBS ONE LIST BROUGHT FORWARD LEAVE ITS STATUS TO THE LAST: bash's
/// `make; fg %vim; fg %less` resumes vim, which is Ctrl-Z'd, then less, which
/// quits before a pass gave it the block, so the list's `D` is less's `fg`'s
/// and make's row takes none, vim living on stopped.
#[test]
fn two_jobs_one_list_brought_forward_leave_its_status_to_the_last() {
    let mut w = World::new();
    w.run_fg(1, 11, "less log", sleeper(1100, "less"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1100).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "make; fg %vim; fg %less", sleeper(1300, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    w.p(1200).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1200;
    let _ = w.tick(TICK_MS);
    w.p(1200).state = ProcState::Stopped;
    w.p(1100).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1100;
    let _ = w.tick(TICK_MS);
    w.exit(1100, false);
    w.close_block(1, 0);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b13").map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, None)),
        "{:?}",
        v.rows
    );
}

/// A DEFERRAL IN ANOTHER TAB LEAVES THIS TAB'S ALONE: two tabs run
/// `make && fg` as their block 13, and the pass that reads one tab's `D`, its
/// vim quit, finds the other tab's list bringing its vim forward under a block
/// of the same id; the first tab's make still takes no status.
#[test]
fn a_deferral_in_another_tab_leaves_this_tabs_alone() {
    let mut w = World::new();
    w.shell_tab(2, 2, 201);
    for (tab, base) in [(1, 1000), (2, 2000)] {
        w.run_fg(tab, 12, "vim notes", sleeper(base + 200, "vim"));
    }
    let _ = run_to(&mut w, 4_000, |_, _| {});
    for (tab, base) in [(1, 1000), (2, 2000)] {
        w.p(base + 200).state = ProcState::Stopped;
        w.stop_block(tab);
    }
    let _ = w.tick(TICK_MS);
    for (tab, base) in [(1, 1000), (2, 2000)] {
        w.run_fg(tab, 13, "make && fg", sleeper(base + 300, "make"));
    }
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1300, false);
    w.p(1200).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1200;
    let _ = w.tick(TICK_MS);
    w.exit(1200, false);
    w.close_block(1, 1);
    w.exit(2300, false);
    w.p(2200).state = ProcState::Sleeping;
    w.tab(2).tpgid = 2200;
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.rows
            .iter()
            .find(|r| r.tab == 1 && r.id == "b13")
            .map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, None)),
        "{:?}",
        v.rows
    );
}

/// A TOOL CALL SEEN IN ITS AGENT'S GROUP BEFORE IT DETACHED LEAVES THE
/// AGENT'S JOB A SHELL JOB: a pass that reads Claude Code's child between its
/// fork and its `setsid` holds it in the agent's block's job, and the call,
/// detached, is no group in front for that job to be found by.
#[test]
fn a_tool_call_seen_in_its_agents_group_before_it_detached_leaves_the_agents_job_a_shell_job() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.agent(1, 300);
    let mut child = Proc::new(400, 300, "zsh");
    child.pgid = 300;
    child.sid = shell;
    child.tty = true;
    w.add(child);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(400).pgid = 400;
    w.p(400).sid = 400;
    w.p(400).tty = false;
    let v = run_to(&mut w, 8_000, |_, _| {});
    assert_eq!(
        v.row("b1").map(|r| r.kind),
        Some(JobKind::Shell),
        "{:?}",
        v.rows
    );
}

/// A SURVIVOR THAT EXITED BEFORE THE PASS READ IT LEAVES NO ORPHAN: the sweep
/// listed make's compiler, which exited before the pass read it, at the pass
/// that read make's `D`.
#[test]
fn a_survivor_that_exited_before_the_pass_read_it_leaves_no_orphan() {
    let mut w = World::new();
    w.run_fg(1, 7, "make", sleeper(1000, "make"));
    let mut cc = Proc::new(1001, 1000, "cc");
    cc.pgid = 1000;
    w.run_bg(1, cc);
    w.p(1001).ppid = 1000;
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.unread.insert(1001);
    w.close_block(1, 0);
    let _ = w.tick(TICK_MS);
    w.exit(1001, false);
    w.unread.clear();
    let v = run_to(&mut w, 2 * TICK_MS, |_, _| {});
    assert!(
        v.rows.iter().all(|r| !r.id.starts_with('o')),
        "{:?}",
        v.rows
    );
}

/// A COMMAND TYPED AHEAD OF ONE NO TICK SAW IN FRONT IS ITS OWN BLOCK'S JOB:
/// block 7 (a builtin, or a command shorter than a pass) never had a group of
/// its own in front, so a tick that read the tab before its `D` took the next
/// command's group for block 7's own. Block 7 ended in no stop, so block 8
/// resumes nothing: block 7's job exits with its status and the next command
/// is block 8's job.
#[test]
fn a_command_typed_ahead_of_one_no_tick_saw_in_front_is_its_own_blocks_job() {
    let mut w = World::new();
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 7,
        started_ms: at,
        open: true,
        command: "read -r x".to_owned(),
        status: None,
        stopped: false,
    });
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.run_bg(1, sleeper(1000, "make"));
    w.tab(1).tpgid = 1000;
    let _ = w.tick(TICK_MS);
    w.close_block(1, 0);
    w.run_fg(1, 8, "make", sleeper(1001, "unused"));
    w.exit(1001, false);
    w.tab(1).tpgid = 1000;
    for _ in 0..2 {
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b7").map(|r| (r.verdict, r.exit)),
            Some((Verdict::Exited, Some(0))),
            "{:?}",
            v.rows
        );
        let row = v.row("b8").expect("make's own job");
        assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
        assert!(row.wire().contains(" exe=make "), "{}", row.wire());
    }
}

/// CTRL-Z AND `bg` NO TICK SAW PARK THE ONE JOB: by the next tick its block
/// ended in a stop and its command runs on in the background. The job keeps
/// its id and quiet clock as a background job, never exits with the stop's
/// status nor leaves its program an orphan. The shell busy with the stop a
/// tick before the reader fed `D`, then leaving the job alive, is no polling.
#[test]
fn ctrl_z_and_bg_no_tick_saw_park_the_one_job() {
    let mut w = World::new();
    w.run_fg(1, 7, "curl x", curl(1000));
    let v = run_to(&mut w, 60_000, |_, _| {});
    let q = v.row("b7").and_then(|r| r.quiet_ms).expect("quiet");
    let shell = w.tab(1).tab.shell_pid;
    w.p(shell).syscalls_per_s = 50;
    let _ = w.tick(TICK_MS);
    w.p(shell).syscalls_per_s = 0;
    w.stop_block(1);
    w.run_fg(1, 8, "bg", sleeper(1001, "unused"));
    w.exit(1001, false);
    w.close_block(1, 0);
    for _ in 0..2 {
        let v = w.tick(TICK_MS);
        let row = v.row("b7").expect("the one job");
        assert_eq!(
            (row.kind, row.verdict == Verdict::Exited),
            (JobKind::ShellBg, false),
            "{}",
            row.wire()
        );
        assert_ne!(row.class, Class::Polling, "{}", row.wire());
        assert!(row.quiet_ms.is_some_and(|n| n > q), "{}", row.wire());
        assert!(
            v.rows.iter().all(|r| !r.id.starts_with('o')),
            "{:?}",
            v.rows
        );
    }
}

/// `fg` NEVER NAMES THE JOB IT RESUMES: not a parked block's job (a tick may
/// see its group in front before the reader fed `fg`'s `C`, even the tick
/// after the one that saw the stop), nor a `cmd &` job, nor a group a tick saw
/// in front before any block and then stopped. The job keeps its own command,
/// so it is never judged by (nor teaches) the `fg` command's class.
#[test]
fn fg_never_names_the_job_it_resumes() {
    for setup in ["parked block", "cmd &", "stopped before a block"] {
        let mut w = World::new();
        for _ in 0..OWN_MIN {
            w.jobs.learn(&command_class("fg", JobKind::Shell), 8_000);
        }
        let id = match setup {
            "parked block" => {
                w.run_fg(1, 12, "curl x", curl(1200));
                let _ = run_to(&mut w, 4_000, |_, _| {});
                w.p(1200).state = ProcState::Stopped;
                w.stop_block(1);
                let _ = w.tick(TICK_MS);
                w.p(1200).state = ProcState::Sleeping;
                w.tab(1).tpgid = 1200;
                let _ = w.tick(TICK_MS);
                "b12"
            }
            "cmd &" => {
                w.run_bg(1, curl(1200));
                let _ = w.tick(TICK_MS);
                "j1200"
            }
            _ => {
                w.run_bg(1, curl(1200));
                w.tab(1).tpgid = 1200;
                let _ = w.tick(TICK_MS);
                w.p(1200).state = ProcState::Stopped;
                let shell = w.tab(1).tab.shell_pid;
                w.tab(1).tpgid = shell;
                let _ = w.tick(TICK_MS);
                w.p(1200).state = ProcState::Sleeping;
                "j1200"
            }
        };
        w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
        w.exit(1201, false);
        w.tab(1).tpgid = 1200;
        let v = run_to(&mut w, 60_000, |_, _| {});
        let row = v.row(id).expect("the resumed job");
        assert_eq!(row.mu_from, Some(MuFrom::Prior), "{setup}: {}", row.wire());
    }
}

/// A BLOCK'S OWN `C` NEVER VOIDS ITS JOB'S LINES: the reader may feed `C`
/// after a pass's clock, so the block starts, and the shell's mark is
/// stamped, past the pass that first finds it. The job's next tick still
/// credits the lines its command printed after `C`.
#[test]
fn a_blocks_own_c_fed_after_a_pass_never_voids_its_jobs_lines() {
    let mut w = World::new();
    w.run_fg(1, 9, "ping -i 1 x", sleeper(1000, "ping"));
    let c = w.at + 1;
    let t = w.tab(1);
    t.tab.block.as_mut().expect("ping's block").started_ms = c;
    t.tab.shell_ms = Some(c);
    let _ = w.observe();
    w.advance(TICK_MS);
    w.line(1);
    let v = w.observe();
    let row = v.row("b9").expect("ping's job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
}

/// A BACKGROUND JOB `fg` RESUMED IS ITS GROUP'S OWN: `make &` was never in
/// front under a block, so the block `fg` resumes it under records its group
/// as the job's own, and a stop and `fg` no tick saw keep the one job.
#[test]
fn a_background_job_fg_resumed_keeps_its_group_through_a_stop_and_fg_no_tick_saw() {
    let mut w = World::new();
    w.run_bg(1, curl(1000));
    let _ = run_to(&mut w, 10_000, |_, _| {});
    w.run_fg(1, 8, "fg", sleeper(1001, "unused"));
    w.exit(1001, false);
    w.tab(1).tpgid = 1000;
    let _ = w.tick(TICK_MS);
    w.stop_block(1);
    w.run_fg(1, 9, "fg", sleeper(1002, "unused"));
    w.exit(1002, false);
    w.tab(1).tpgid = 1000;
    let v = w.tick(TICK_MS);
    assert!(v.row("b9").is_none(), "{:?}", v.rows);
    let row = v.row("j1000").expect("the one job");
    assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
}

/// A STOP AND `fg` NO TICK SAW KEEP AN `a && b` BLOCK ONE JOB: the stopped
/// command is the block's second group, not its first, and is in front
/// again under the `fg` block, also when a tick saw the shell take the
/// terminal back before the reader fed the stop's `D`.
#[test]
fn a_stop_and_fg_no_tick_saw_keep_a_later_command_of_a_block_its_jobs() {
    for before_d in [false, true] {
        let mut w = World::new();
        w.run_fg(1, 51, "make && make test", sleeper(1000, "make"));
        let _ = run_to(&mut w, 6_000, |_, _| {});
        w.exit(1000, false);
        let shell = w.tab(1).tab.shell_pid;
        w.tab(1).tpgid = shell;
        let _ = w.tick(TICK_MS);
        w.run_bg(1, sleeper(1001, "make"));
        w.tab(1).tpgid = 1001;
        let _ = w.tick(TICK_MS);
        if before_d {
            w.p(1001).state = ProcState::Stopped;
            w.tab(1).tpgid = shell;
            let _ = w.tick(TICK_MS);
            w.p(1001).state = ProcState::Sleeping;
        }
        w.stop_block(1);
        w.run_fg(1, 52, "fg", sleeper(1002, "unused"));
        w.exit(1002, false);
        w.tab(1).tpgid = 1001;
        let v = w.tick(TICK_MS);
        assert!(v.row("b52").is_none(), "{before_d}: {:?}", v.rows);
        let row = v.row("b51").expect("the one job");
        assert_ne!(row.verdict, Verdict::Exited, "{before_d}: {}", row.wire());
    }
}

/// `fg`'S BLOCK READ BEFORE THE SHELL HANDS THE TERMINAL OVER RESUMES THE
/// PARKED JOB: a tick finds `fg`'s block with the shell alone in front, and
/// the tick that sees the parked group in front makes it that block's job,
/// one job that takes the lines it prints.
#[test]
fn fgs_block_read_before_the_shell_hands_the_terminal_over_resumes_the_parked_job() {
    let mut w = World::new();
    w.run_fg(1, 12, "curl x", curl(1200));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    let shell = w.tab(1).tab.shell_pid;
    w.tab(1).tpgid = shell;
    let _ = w.tick(TICK_MS);
    w.tab(1).tpgid = 1200;
    w.p(1200).state = ProcState::Sleeping;
    let v = run_to(&mut w, 10_000, |w, _| w.line(1));
    assert!(v.row("b13").is_none(), "{:?}", v.rows);
    let row = v.row("b12").expect("the resumed job");
    assert_eq!(
        (row.kind, row.class),
        (JobKind::Shell, Class::Running),
        "{}",
        row.wire()
    );
}

/// `fg`'S NOTICE AFTER A PASS'S CLOCK IS NONE OF THE JOB'S PROGRESS: the
/// pass that resumes a parked job can take its clock between `fg`'s `C` and
/// the shell's notice, so the notice lands in the job's next tick, which
/// credits it nothing either, also when the pass read the group in front
/// well after its clock (a slow sweep). A line the job prints after the
/// notice is its own however long that next tick comes after (a slow
/// cadence).
#[test]
fn fgs_notice_after_the_resuming_pass_clock_is_none_of_the_jobs_progress() {
    for (lag, notice, own_line) in [(0, 5, false), (1_500, 1_200, false), (0, 5, true)] {
        let mut w = World::new();
        w.run_fg(1, 12, "curl x", curl(1200));
        let _ = run_to(&mut w, 4_000, |_, _| {});
        w.p(1200).state = ProcState::Stopped;
        w.stop_block(1);
        let _ = w.tick(TICK_MS);
        let c = w.at + 1;
        w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
        w.exit(1201, false);
        w.tab(1).tab.block.as_mut().expect("fg's block").started_ms = c;
        w.tab(1).tab.shell_ms = Some(c);
        w.tab(1).tpgid = 1200;
        w.p(1200).state = ProcState::Sleeping;
        w.read_lag_ms = lag;
        let _ = w.tick(TICK_MS);
        w.read_lag_ms = 0;
        w.advance(notice);
        w.line(1);
        if own_line {
            w.advance(30_000);
            w.line(1);
        } else {
            w.advance(TICK_MS - notice);
        }
        let v = w.observe();
        let row = v.row("b12").expect("the resumed job");
        assert_eq!(
            row.class == Class::Running,
            own_line,
            "{own_line}: {}",
            row.wire()
        );
    }
}

#[test]
fn fg_makes_a_parked_job_the_next_blocks_foreground_job_again() {
    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.p(shell).counters.cpu_ns = 5_000_000_000;
    w.p(shell).counters.logical_writes = Some(64 << 20);
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let v = run_to(&mut w, 10_000, |_, _| {});
    assert_eq!(v.row("b12").map(|r| r.verdict), Some(Verdict::Parked));
    let c = w.at + 1;
    w.tab(1).tab.block = Some(Block {
        id: 13,
        started_ms: c,
        open: true,
        command: "fg".to_owned(),
        status: None,
        stopped: false,
    });
    w.tab(1).tpgid = 1200;
    w.p(1200).state = ProcState::Sleeping;
    w.tty_mode(1, false, false);
    w.tab(1).tab.shell_ms = Some(c);
    w.advance(TICK_MS);
    w.line(1);
    let v = w.observe();
    assert!(v.row("b13").is_none(), "{:?}", v.rows);
    let row = v.row("b12").expect("the resumed job keeps its id");
    assert_ne!(
        row.class,
        Class::Running,
        "the shell's notice after fg's C: {}",
        row.wire()
    );
    assert_eq!(
        (row.kind, row.consumer),
        (JobKind::Shell, Consumer::PersonFg)
    );
    assert_eq!(row.wait.as_ref().map(|w| w.name), Some(WaitName::TtyRaw));
    let v = w.tick(TICK_MS);
    assert!(v.row("b13").is_none(), "nor at the next tick: {:?}", v.rows);
    assert_ne!(
        v.row("b12").map(|r| r.class),
        Some(Class::Running),
        "the shell rejoining it is no birth"
    );
    w.exit(1200, false);
    w.close_block(1, 0);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b12").map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, Some(0)))
    );
}

#[test]
fn an_idle_dev_server_with_a_keep_alive_client_listens_and_never_reads_stuck() {
    let s = scenarios::named("devserver.pty");
    let t = scenarios::run(&s, None);
    let bad = scenarios::check(&s, &t);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let (_, row, _) = t.ticks.last().expect("ticks");
    let row = row.as_ref().expect("the server");
    assert!(
        row.wire().contains("wait=listen port=5173"),
        "{}",
        row.wire()
    );
}

#[test]
fn a_connection_is_the_wait_when_the_queue_waits_on_it_alone() {
    let mut w = World::new();
    let mut p = curl(1000);
    p.fds.push(aterm_vitals::jobs::Fd {
        fd: 7,
        kind: aterm_vitals::jobs::FdKind::EventQueue {
            sleeping: Some(true),
            knotes: Some(vec![aterm_vitals::jobs::Knote {
                ident: 5,
                filter: aterm_vitals::jobs::KnoteFilter::Read,
            }]),
        },
    });
    w.run_fg(1, 51, "node fetch.js", p);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let row = v.row("b51").expect("row");
    assert_eq!(row.wait.as_ref().map(|w| w.name), Some(WaitName::Tcp));
    assert_eq!(row.verdict, Verdict::Stuck);
    let mut w = World::new();
    let mut p = curl(1000);
    p.fds.push(aterm_vitals::jobs::Fd {
        fd: 7,
        kind: aterm_vitals::jobs::FdKind::EventQueue {
            sleeping: Some(true),
            knotes: Some(vec![
                aterm_vitals::jobs::Knote {
                    ident: 5,
                    filter: aterm_vitals::jobs::KnoteFilter::Read,
                },
                aterm_vitals::jobs::Knote {
                    ident: 1,
                    filter: aterm_vitals::jobs::KnoteFilter::Timer,
                },
            ]),
        },
    });
    w.run_fg(1, 51, "node pool.js", p);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let row = v.row("b51").expect("row");
    assert_eq!(row.wait.as_ref().map(|w| w.name), Some(WaitName::EventLoop));
    assert_eq!(row.verdict, Verdict::Quiet);
}

#[test]
fn a_full_screen_program_waking_often_with_no_key_queued_reads_keys_not_polling() {
    let s = scenarios::named("tuiidle.pty");
    let t = scenarios::run(&s, None);
    let bad = scenarios::check(&s, &t);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    let mut w = World::new();
    let mut p = sleeper(1000, "poller");
    p.syscalls_per_s = 1_100;
    w.run_fg(1, 51, "./poller", p);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(v.row("b51").map(|r| r.class), Some(Class::Polling));
}

#[test]
fn a_program_at_the_persons_raw_read_parks_demand_even_while_it_writes_files() {
    let writer = || {
        let mut p = sleeper(1000, "agy");
        p.cpu_per_s = 18_000_000;
        p.writes_per_s = 256 * 1024;
        p
    };
    let class = |v: &JobsView| v.row("b51").map(|r| r.class);

    let mut w = World::new();
    w.run_fg(1, 51, "./agy", writer());
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(class(&v), Some(Class::Running), "its writes are progress");
    assert_eq!(v.demand, Demand::Idle, "at the person's raw read: parked");

    let mut w = World::new();
    w.run_fg(1, 51, "./agy", writer());
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(class(&v), Some(Class::Running));
    assert_eq!(v.demand, Demand::Fast, "a canonical terminal names no read");

    let mut w = World::new();
    w.run_fg(1, 51, "./agy", writer());
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 60_000, |w, rel| {
        if rel > 0 {
            w.cr(1);
        }
    });
    assert_eq!(class(&v), Some(Class::Running));
    assert_eq!(
        v.demand,
        Demand::Fast,
        "drawn past the word: it names nothing"
    );

    let mut w = World::new();
    w.run_fg(1, 51, "./agy", spinner(1000, "agy"));
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(class(&v), Some(Class::BusySilent));
    assert_eq!(
        v.demand,
        Demand::Fast,
        "spinning at the raw read: still watched"
    );
}

#[test]
fn a_tab_is_parked_only_while_every_live_job_waits_at_the_persons_read() {
    let parked = |w: &mut World| run_to(w, 20_000, |_, _| {}).parked_tabs;

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", sleeper(1000, "muse"));
    w.tty_mode(1, false, false);
    w.shell_tab(2, 2, 2000);
    assert_eq!(parked(&mut w), vec![1], "at its prompt: parked");

    let mut w = World::new();
    let mut writer = sleeper(1000, "agy");
    writer.cpu_per_s = 18_000_000;
    writer.writes_per_s = 256 * 1024;
    w.run_fg(1, 51, "./agy", writer);
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(v.row("b51").map(|r| r.class), Some(Class::Running));
    assert_eq!(v.parked_tabs, vec![1], "writing its own files at the read");

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", sleeper(1000, "muse"));
    assert!(
        parked(&mut w).is_empty(),
        "a canonical terminal names no read"
    );

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", sleeper(1000, "muse"));
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 20_000, |w, rel| {
        if rel > 0 {
            w.cr(1);
        }
    });
    assert!(
        v.parked_tabs.is_empty(),
        "drawn past its word: no read named"
    );

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", spinner(1000, "muse"));
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(v.demand, Demand::Fast);
    assert!(v.parked_tabs.is_empty(), "spinning at the read: watched");

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", sleeper(1000, "muse"));
    w.tty_mode(1, false, false);
    w.run_bg(1, sleeper(1100, "sleep"));
    assert!(parked(&mut w).is_empty(), "a background job beside it");

    let mut w = World::new();
    w.run_fg(1, 51, "./muse", sleeper(1000, "muse"));
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.close_block(1, 0);
    let v = run_to(&mut w, 4_000, |_, _| {});
    assert_eq!(v.row("b51").map(|r| r.verdict), Some(Verdict::Exited));
    assert!(
        v.parked_tabs.is_empty(),
        "an exited job alone parks nothing"
    );
}

fn logging_tui(w: &mut World, canonical: bool) {
    let log = (1, 77_051);
    w.files.insert(
        log,
        OutFile {
            size: 0,
            per_s: 40,
            lf: Some(true),
        },
    );
    let mut agy = sleeper(5100, "agy");
    agy.out = Some(log);
    w.run_fg(1, 51, "agy", agy);
    w.tty_mode(1, canonical, canonical);
}

#[test]
fn a_full_screen_program_logging_to_its_own_file_parks_demand() {
    let mut w = World::new();
    logging_tui(&mut w, false);
    let v = run_to(&mut w, 30_000, |_, _| {});
    let row = v.row("b51").expect("the program's job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
    assert_eq!(v.demand, Demand::Idle, "at the person's raw read: parked");
    let mut w = World::new();
    logging_tui(&mut w, true);
    let v = run_to(&mut w, 30_000, |_, _| {});
    let row = v.row("b51").expect("the command's job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
    assert_eq!(v.demand, Demand::Fast, "a canonical terminal names no read");
    let mut w = World::new();
    logging_tui(&mut w, false);
    let v = run_to(&mut w, 30_000, |w, rel| {
        if rel == 2_000 {
            w.cr(1);
        }
    });
    let row = v.row("b51").expect("the program's job");
    assert_eq!(row.class, Class::Running, "{}", row.wire());
    assert_eq!(
        v.demand,
        Demand::Fast,
        "drawn past the word: it names nothing"
    );
}

#[test]
fn a_process_name_prints_only_as_a_safe_word() {
    assert!(is_exe_word("cargo"));
    assert!(is_exe_word("clang++"));
    assert!(is_exe_word("build-script-build"));
    assert!(is_exe_word("python3.12"));
    for bad in [
        "",
        "two words",
        "a=b",
        "x\u{1b}]0;pwn\u{7}",
        "\u{202e}rlo",
        "caf\u{e9}",
        "quote\"d",
        "semi;colon",
        "abcdefghijklmnopqrstuvwxyz0123456",
    ] {
        assert!(!is_exe_word(bad), "{bad:?}");
        assert_eq!(exe_word(bad), "-");
    }
}

#[test]
fn foreign_names_never_reach_the_row_or_the_sentence_raw() {
    let mut w = World::new();
    let hostile = "x\u{1b}]0;pwn\u{7}\u{202e}y";
    let mut p = sleeper(1000, hostile);
    p.state = ProcState::Stopped;
    w.run_fg(1, 51, "x", p);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let row = v.row("b51").expect("row");
    let text = row.wire();
    assert!(
        text.contains(" exe=- ") && text.contains("stopped_pid=1000:- "),
        "{text}"
    );
    let printable = |t: &str| {
        t.chars()
            .all(|c| !c.is_control() && !('\u{2000}'..='\u{206f}').contains(&c))
    };
    assert!(printable(&text), "{text:?}");
    assert!(row.verdict.is_suspect(), "{text}");
    let said = row.said();
    assert!(!said.is_empty() && printable(&said), "{said:?}");
    for line in v.findings.wire_lines() {
        assert!(printable(&line), "{line:?}");
    }
    let mut w = World::new();
    w.call(
        1,
        300,
        400,
        sleeper(401, "sleep"),
        Some(hint(400, Consumer::AgentBg, Some("bad id;rm"), None)),
    );
    let v = w.tick(TICK_MS);
    assert!(
        wire(&v, "a400").contains(" task=- "),
        "{}",
        wire(&v, "a400")
    );
}

#[test]
fn the_row_names_the_executable_not_its_cut_short_name() {
    let mut w = World::new();
    let mut p = sleeper(1000, "very-long-tool-n");
    p.exe = Some("very-long-tool-name".to_owned());
    w.run_fg(1, 51, "very-long-tool-name", p);
    let v = w.tick(TICK_MS);
    assert!(
        wire(&v, "b51").contains(" exe=very-long-tool-name "),
        "{}",
        wire(&v, "b51")
    );
}

#[test]
fn an_agent_calls_class_is_the_command_its_join_names_never_its_shell_wrapper() {
    let quick = |w: &mut World, leader: u32, command: Option<&str>| {
        let mut h = hint(leader, Consumer::AgentBg, None, None);
        h.command = command.map(str::to_owned);
        w.call(1, 300, leader, spinner(leader + 1, "cargo"), Some(h));
        run_to(w, 20_000, |w, _| w.call_file(leader).size += 40);
        w.exit(leader + 1, false);
        w.exit(leader, false);
        run_to(w, TICK_MS, |_, _| {});
    };
    let mut w = World::new();
    for n in 0..3 {
        quick(&mut w, 400 + 10 * n, None);
        quick(&mut w, 500 + 10 * n, Some("cargo test -p aterm-vitals"));
    }
    let (wrapper, unnamed) = (
        command_class("zsh", JobKind::AgentCall),
        command_class("", JobKind::AgentCall),
    );
    assert_eq!(
        (w.jobs.classes().mu(&wrapper), w.jobs.classes().mu(&unnamed)),
        (None, None),
        "calls no join names teach no class"
    );
    assert!(
        w.jobs
            .classes()
            .mu(&command_class("cargo test", JobKind::AgentCall))
            .is_some(),
        "a joined command teaches its own"
    );
    for _ in 0..3 {
        w.jobs.learn(&wrapper, 10_000);
        w.jobs.learn(&unnamed, 10_000);
    }
    w.call(1, 300, 600, sleeper(601, "sleep"), None);
    let v = run_to(&mut w, 240_000, |_, _| {});
    let row = v.row("a600").expect("the sleep");
    assert_eq!(row.mu_from, Some(MuFrom::Prior), "{}", row.wire());
    assert!(!row.no_progress, "{}", row.wire());
}

/// `while sleep 2; do date; done` run by tab 1's shell under block `id`: each tick the shell
/// forks the next short command and reaps the last, and every `date` prints its line.
fn date_loop(w: &mut World, id: u64) -> impl FnMut(&mut World, u64) + use<> {
    let shell = w.tab(1).tab.shell_pid;
    let at = w.at;
    let t = w.tab(1);
    if let Some(mut b) = t.tab.block.take() {
        b.open = false;
        t.tab.ended = Some(b);
    }
    t.tab.block = Some(Block {
        id,
        started_ms: at,
        open: true,
        command: "while sleep 2; do date; done".to_owned(),
        status: None,
        stopped: false,
    });
    move |w: &mut World, rel: u64| {
        let born = 3_000 + u32::try_from(rel / TICK_MS).unwrap_or(0);
        w.exit(born - 1, false);
        let mut p = Proc::new(born, shell, if born % 2 == 0 { "date" } else { "sleep" });
        p.sid = shell;
        p.tty = true;
        w.add(p);
        w.tab(1).tpgid = born;
        *w.p(shell).counters.syscalls.get_or_insert(0) += 3;
        w.line_from(1, &[]);
    }
}

/// A ZOMBIE WRITES NOTHING: Ctrl-Z of `ninja` leaves its finished steps unreaped, each in the
/// group ninja made for it. A loop run after takes its own lines, and a `make check &` the one
/// background job that can print.
#[test]
fn a_zombie_writes_nothing() {
    for sole in [false, true] {
        let mut w = World::new();
        let shell = w.tab(1).tab.shell_pid;
        w.mark(1);
        w.run_fg(1, 7, "ninja", sleeper(1300, "ninja"));
        let mut step = Proc::new(1301, 1300, "cc");
        step.sid = shell;
        step.tty = true;
        w.add(step);
        let _ = run_to(&mut w, 10_000, |w, _| w.line_from(1, &[1300, 1301]));
        w.p(1300).state = ProcState::Stopped;
        w.exit(1301, true);
        w.stop_block(1);
        w.mark(1);
        let _ = w.tick(TICK_MS);
        let (id, v) = if sole {
            w.run_bg(1, sleeper(1400, "make"));
            (
                "j1400",
                run_to(&mut w, 600_000, |w, _| w.line_from(1, &[1400])),
            )
        } else {
            let each = date_loop(&mut w, 8);
            ("b8", run_to(&mut w, 600_000, each))
        };
        assert_eq!(
            (v.row(id).map(|r| r.class), v.row("b7").map(|r| r.verdict)),
            (Some(Class::Running), Some(Verdict::Parked)),
            "{sole}: {:?}",
            v.rows.iter().map(|r| r.wire()).collect::<Vec<_>>()
        );
    }
}

/// A PROCESS TICKING AT ITS IDLE RATE WROTE NOTHING: a runtime's timers wake it as often over a
/// tick the tab printed nothing in as over one it printed in. A JVM deadlocked in front takes
/// none of a `tail -f &`'s lines, and an idle `python3 -m http.server &` leaves a loop run after
/// it its own.
#[test]
fn a_process_ticking_at_its_idle_rate_wrote_nothing() {
    let mut w = World::new();
    w.run_bg(1, sleeper(1400, "tail"));
    let mut java = sleeper(1300, "java");
    java.syscalls_per_s = 40;
    w.run_fg(1, 9, "java -jar app.jar", java);
    w.tty_mode(1, true, true);
    let v = run_to(&mut w, 1_200_000, |w, rel| {
        if rel % 60_000 == 0 {
            w.line_from(1, &[1400]);
        }
    });
    assert_eq!(
        v.row("b9").map(|r| r.quiet_ms),
        Some(Some(1_200_000)),
        "{}",
        wire(&v, "b9")
    );

    let mut w = World::new();
    let mut server = sleeper(1400, "python3");
    server.syscalls_per_s = 2;
    server.fds.push(socket_fd(3));
    w.run_bg(1, server);
    w.mark(1);
    let _ = run_to(&mut w, 10_000, |_, _| {});
    let each = date_loop(&mut w, 9);
    let v = run_to(&mut w, 600_000, each);
    assert_eq!(
        v.row("b9").map(|r| r.class),
        Some(Class::Running),
        "{}",
        wire(&v, "b9")
    );
}

/// A NESTED SESSION THE PERSON'S KEYS REACH STAYS SO: once two wakes of its reader at the
/// person's keys showed it relays them, keys it does not read change nothing. `nvim`'s terminal
/// keeps its shell's raw read while the person types in another window; `vim` in `script`'s
/// shell, whose plugins' timers wake it, latches on keys that wake it past that rate; and an `ssh`
/// in Emacs's shell buffer, whose keys Emacs holds until Enter, latches on two Enters 40 s apart.
/// An agent's pty call its keys woke by chance twice, two minutes apart, never latches.
#[test]
fn a_nested_session_the_persons_keys_reach_stays_so() {
    let mut w = World::new();
    w.run_fg(1, 5, "nvim", sleeper(1300, "nvim"));
    let mut term = Proc::new(1310, 1300, "zsh");
    term.sid = 1310;
    term.tty = true;
    w.add(term);
    w.fronts.insert(1310, 1310);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    typed_into(&mut w, 1310);
    let v = run_to(&mut w, 60_000, |w, _| w.typed(1, 1300));
    assert_eq!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyRaw),
        "{}",
        wire(&v, "n1310")
    );

    // `vim` in `script`'s shell, its plugins' timers waking it: the keys wake it past that rate.
    let mut w = World::new();
    w.run_fg(1, 5, "script -q log", sleeper(1300, "script"));
    let mut inner = Proc::new(1310, 1300, "zsh");
    inner.sid = 1310;
    inner.tty = true;
    w.add(inner);
    let mut vim = Proc::new(1311, 1310, "vim");
    vim.sid = 1310;
    vim.tty = true;
    vim.syscalls_per_s = 5;
    w.add(vim);
    w.fronts.insert(1310, 1311);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 10_000, |_, _| {});
    typed_into(&mut w, 1311);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("n1310").and_then(|r| r.wait.as_ref().map(|w| w.name)),
        Some(WaitName::TtyRaw),
        "{}",
        wire(&v, "n1310")
    );

    // A Codex call in a pty of its own, hung with its terminal raw: the person's keys to Codex
    // wake it by chance twice, two minutes apart, which shows nothing.
    let mut w = World::new();
    w.run_fg(1, 5, "codex", sleeper(1300, "codex"));
    let mut call = Proc::new(1301, 1300, "bash");
    call.sid = 1301;
    call.tty = true;
    w.add(call);
    let mut test = Proc::new(1302, 1301, "node");
    test.sid = 1301;
    test.tty = true;
    test.fds.push(tcp_fd(5));
    w.add(test);
    w.fronts.insert(1301, 1302);
    w.terms.insert(
        1301,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    let v = run_to(&mut w, 1_200_000, |w, rel| match rel {
        100_000 | 220_000 => w.typed(1, 1302),
        r if r % 10_000 == 0 => w.typed(1, 1300),
        _ => {}
    });
    assert_eq!(
        v.row("n1301").map(|r| r.verdict),
        Some(Verdict::Stuck),
        "{}",
        wire(&v, "n1301")
    );

    let mut w = World::new();
    w.run_fg(1, 5, "emacs", sleeper(1300, "emacs"));
    let mut sh = Proc::new(1310, 1300, "bash");
    sh.sid = 1310;
    sh.tty = true;
    w.add(sh);
    let mut ssh = Proc::new(1311, 1310, "ssh");
    ssh.sid = 1310;
    ssh.tty = true;
    ssh.fds.push(tcp_fd(3));
    w.add(ssh);
    w.fronts.insert(1310, 1311);
    w.terms.insert(
        1310,
        TermWord {
            canonical: false,
            echo: false,
            queued: 0,
        },
    );
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 80_000, |w, rel| match rel {
        20_000 | 60_000 => w.typed(1, 1311),
        r if r > 0 && r % 4_000 == 0 => w.typed(1, 1300),
        _ => {}
    });
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("n1310")
            .map(|r| (r.verdict, r.wait.as_ref().map(|w| w.name))),
        Some((Verdict::Fine, Some(WaitName::TtyRaw))),
        "{}",
        wire(&v, "n1310")
    );
}

/// A NESTED SESSION'S LINE READ IS ITS LEADER'S ALONE, WAITING ON NOTHING ELSE: a REPL in Emacs's
/// buffer hung on its socket, or a shell there stuck in the kernel on a dead share, reads that
/// wait; and a shell there at its prompt beside a background job hung on its peer is no line read.
#[test]
fn a_nested_line_read_is_the_leaders_alone_waiting_on_nothing_else() {
    for (name, hang) in [("python3", 0), ("bash", 1), ("zsh", 2)] {
        let mut w = World::new();
        w.run_fg(1, 5, "emacs", sleeper(1300, "emacs"));
        let mut lead = Proc::new(1310, 1300, name);
        lead.sid = 1310;
        lead.tty = true;
        if hang == 0 {
            lead.fds.push(tcp_fd(5));
        }
        w.add(lead);
        if hang == 2 {
            let mut get = curl(1312);
            get.ppid = 1310;
            get.sid = 1310;
            get.tty = true;
            w.add(get);
        }
        w.fronts.insert(1310, 1310);
        w.terms.insert(
            1310,
            TermWord {
                canonical: true,
                echo: true,
                queued: 0,
            },
        );
        w.tty_mode(1, false, false);
        typed_into(&mut w, 1310);
        w.p(1310).d_state = hang == 1;
        let v = run_to(&mut w, 1_200_000, |_, _| {});
        assert_eq!(
            v.row("n1310").map(|r| r.verdict),
            Some(Verdict::Stuck),
            "{name}: {:?}",
            v.rows.iter().map(|r| r.wire()).collect::<Vec<_>>()
        );
    }
}

/// THE TAB'S ROOT DOES JOB CONTROL WHILE IT IS THE SHELL A PASS SAW AT ITS PROMPT: in a tab with
/// no marks, zsh's line editor reading raw keys in front shows it, and a `git pull &` stopped at
/// its credential prompt parks. After `exec ninja` the root is ninja, which does none: a step it
/// stopped in a group of its own is held, and the block reads no line of the shell's.
#[test]
fn the_tabs_root_does_job_control_while_it_is_the_shell_a_pass_saw() {
    let mut w = World::new();
    w.tty_mode(1, false, false);
    let mut git = sleeper(1400, "git");
    git.state = ProcState::Stopped;
    w.run_bg(1, git);
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    assert_eq!(
        v.row("j1400").map(|r| r.verdict),
        Some(Verdict::Parked),
        "{}",
        wire(&v, "j1400")
    );

    let mut w = World::new();
    let shell = w.tab(1).tab.shell_pid;
    w.mark(1);
    let _ = w.tick(TICK_MS);
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 7,
        started_ms: at,
        open: true,
        command: "exec ninja".to_owned(),
        status: None,
        stopped: false,
    });
    let root = w.p(shell);
    root.name = "ninja".into();
    root.exec += 1;
    for (pid, ppid, name) in [(2310, shell, "sh"), (2311, 2310, "ssh")] {
        let mut p = Proc::new(pid, ppid, name);
        p.pgid = 2310;
        p.sid = shell;
        p.tty = true;
        p.state = ProcState::Stopped;
        w.add(p);
    }
    let v = run_to(&mut w, 1_200_000, |_, _| {});
    let rows: Vec<String> = v.rows.iter().map(|r| r.wire()).collect();
    assert!(
        v.rows.iter().all(|r| r.verdict != Verdict::Parked
            && r.wait.as_ref().map(|w| w.name) != Some(WaitName::TtyBuiltinRead)),
        "{rows:?}"
    );
}
