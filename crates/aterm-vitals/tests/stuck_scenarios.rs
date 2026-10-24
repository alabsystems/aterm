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
    Named, NamedWait, ProcState, TierB, Verdict, WaitName, command_class,
};
use scenarios::{TICK_MS, curl, follow_fds, pipe_end, sleeper, spinner, tcp_fd};
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
    w.run_bg(1, sleeper(1300, "claude"));
    w.tab(1).tpgid = 1300;
    let v = w.tick(TICK_MS);
    assert_eq!(v.row("j1300").map(|r| r.kind), Some(JobKind::Shell));
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 14,
        started_ms: at,
        open: true,
        command: "claude".to_owned(),
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
/// over: the ended block's job exits with its status and the next command is
/// the next block's job, no orphan.
#[test]
fn the_next_command_typed_ahead_is_its_own_blocks_job() {
    let mut w = World::new();
    w.run_fg(1, 7, "make", sleeper(1000, "make"));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.exit(1000, false);
    w.run_bg(1, sleeper(1001, "less"));
    w.tab(1).tpgid = 1001;
    let _ = w.tick(TICK_MS);
    w.close_block(1, 0);
    w.run_fg(1, 8, "less x", sleeper(1002, "unused"));
    w.exit(1002, false);
    w.tab(1).tpgid = 1001;
    for _ in 0..2 {
        let v = w.tick(TICK_MS);
        assert_eq!(
            v.row("b7").map(|r| (r.verdict, r.exit)),
            Some((Verdict::Exited, Some(0)))
        );
        let row = v.row("b8").expect("the next command's job");
        assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
        assert!(row.wire().contains(" exe=less "), "{}", row.wire());
        assert!(
            v.rows.iter().all(|r| !r.id.starts_with('o')),
            "{:?}",
            v.rows
        );
    }
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

/// `fg` NEVER NAMES THE JOB IT RESUMES: a tick may see a parked job's group
/// in front before the reader fed `fg`'s `C`, even the tick after the one
/// that saw the stop. Once the `fg` block is learned
/// the job keeps its own command, so it is never judged by (nor teaches) the
/// `fg` command's class.
#[test]
fn fg_never_names_the_job_it_resumes() {
    let mut w = World::new();
    w.jobs.learn(&command_class("fg", JobKind::Shell), 8_000);
    w.run_fg(1, 12, "curl x", curl(1200));
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.stop_block(1);
    let _ = w.tick(TICK_MS);
    w.p(1200).state = ProcState::Sleeping;
    w.tab(1).tpgid = 1200;
    let _ = w.tick(TICK_MS);
    w.run_fg(1, 13, "fg", sleeper(1201, "unused"));
    w.exit(1201, false);
    w.tab(1).tpgid = 1200;
    let v = run_to(&mut w, 60_000, |_, _| {});
    let row = v.row("b12").expect("the resumed job");
    assert_eq!(row.mu_from, Some(MuFrom::Prior), "{}", row.wire());
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
/// again under the `fg` block.
#[test]
fn a_stop_and_fg_no_tick_saw_keep_a_later_command_of_a_block_its_jobs() {
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
    w.stop_block(1);
    w.run_fg(1, 52, "fg", sleeper(1002, "unused"));
    w.exit(1002, false);
    w.tab(1).tpgid = 1001;
    let v = w.tick(TICK_MS);
    assert!(v.row("b52").is_none(), "{:?}", v.rows);
    let row = v.row("b51").expect("the one job");
    assert_ne!(row.verdict, Verdict::Exited, "{}", row.wire());
}

/// `fg`'S NOTICE AFTER A PASS'S CLOCK IS NONE OF THE JOB'S PROGRESS: the
/// pass that resumes a parked job can take its clock between `fg`'s `C` and
/// the shell's notice, so the notice lands in the job's next tick, which
/// credits it nothing either.
#[test]
fn fgs_notice_after_the_resuming_pass_clock_is_none_of_the_jobs_progress() {
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
    let _ = w.tick(TICK_MS);
    w.advance(5);
    w.line(1);
    w.advance(TICK_MS - 5);
    let v = w.observe();
    let row = v.row("b12").expect("the resumed job");
    assert_ne!(row.class, Class::Running, "{}", row.wire());
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
