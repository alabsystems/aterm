// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stuck-process engine against the design: the §11.10 example rows
//! (the `sys jobs` row word for word, the contract an agent reads); every
//! scenario of the §18 expectation table in each shape it names, with the
//! shapes an interactive shell adds; and the laws of §11.3–§11.9 one at a
//! time (orphans, the exited row, Ctrl-Z's carried clock and `fg`, a block
//! as one job, demand, findings, `serves`, lanes off, the wait name's 2-tick
//! agreement, pid reuse, episodes, the class table, the foreign-text rule).

#[path = "support/world.rs"]
mod world;

#[path = "support/scenarios.rs"]
mod scenarios;

use aterm_vitals::jobs::wire::{exe_word, is_exe_word};
use aterm_vitals::jobs::{
    Block, Class, Consumer, Demand, GateWord, JobKind, JobsView, MuFrom, NamedWait, ProcState,
    TierB, Verdict, WaitName, command_class,
};
use scenarios::{TICK_MS, curl, follow_fds, pipe_end, sleeper, spinner, tcp_fd};
use world::{OutFile, Proc, World, hint};

/// Tick every 2 s from now until `until_ms` from now, calling `each` after
/// each advance; the last view.
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

// §11.10 example rows

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
    // A person's wait arms nothing.
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
    // Tab 2's cargo holds the lock; its rustc children churn with work.
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
    // φ passed 8 at 3 min 41 s, and an unnamed wait stays capped at quiet.
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
    // One tick of a legitimate class keeps the suspect verdict (no flap)…
    assert_eq!(held.row("b77").map(|r| r.verdict), Some(Verdict::Quiet));
    w.advance(TICK_MS);
    churn(&mut w, 424_000);
    let v = w.observe();
    // …and the second makes it fine: it waits for a running holder.
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

    // The contrast: vim, Ctrl-Z'd, the shell holding the terminal again.
    let mut w = World::new();
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    let v = run_to(&mut w, 7_200_000, |w, rel| {
        if rel == 2_000 {
            w.p(1200).state = ProcState::Stopped;
            w.close_block(1, 148);
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
        // One worker forked per request every 65 s, never reaped.
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

// §18 item 8: every scenario, every shape

#[test]
fn every_scenario_of_the_expectation_table_reads_as_the_table_says() {
    let all = scenarios::all();
    // Rows 1–24 and the two added rows, each in the shapes it names.
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

// Laws

#[test]
fn a_call_whose_leader_exits_leaves_an_orphan_with_findings_only() {
    let mut w = World::new();
    let mut child = spinner(401, "cargo-build-script");
    child.cpu_per_s = 1_000_000_000;
    w.call(1, 300, 400, child, None);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    // The call shell is killed; its build script lives on at ppid 1.
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
    // The child ends; the call's leader exits and its agent has not reaped
    // it yet: a zombie, which leads no session.
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

#[test]
fn ctrl_z_carries_the_jobs_id_and_quiet_clock_into_the_background() {
    let mut w = World::new();
    w.run_fg(1, 51, "./spin", sleeper(1000, "spin"));
    let before = run_to(&mut w, 100_000, |_, _| {});
    let q = before.row("b51").and_then(|r| r.quiet_ms).expect("quiet");
    w.p(1000).state = ProcState::Stopped;
    w.close_block(1, 148);
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
    // The agent is a person-fg job waiting unnamed: a suspect, fast.
    let v = w.tick(TICK_MS);
    assert_eq!(v.demand, Demand::Fast);
    // With the agent's own job gone from the picture, a background follow
    // alone is a legitimate wait: slow.
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
    // aterm, the shell and fsck are ours; the stopped one is root's.
    assert_eq!(v.findings.own_procs, 3);
    assert_eq!(v.findings.process_limit, Some(5333));
    let lines = v.findings.wire_lines();
    assert!(
        lines.contains(&"finding kind=process-table own=3 limit=5333".to_owned()),
        "{lines:?}"
    );
}

#[test]
fn a_detached_job_reading_its_consumers_pipe_serves() {
    let mut w = World::new();
    let mut server = sleeper(401, "mcp-server");
    server.fds.push(pipe_end(0, 0xD1, 0xD2, false, 0x8, 0));
    w.call(1, 300, 400, server, None);
    // The agent holds the write end.
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
    // One tick without the socket: still tcp.
    let fds = std::mem::take(&mut w.p(1000).fds);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Tcp)
    );
    // A second agreeing tick: unnamed.
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b51").and_then(|r| r.wait.as_ref()).map(|w| w.name),
        Some(WaitName::Unnamed)
    );
    // Back for one tick: unnamed holds; two: tcp.
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
    // The call ends; a new call reuses both pids with new start times.
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
    // `exited` names both a verdict and a class (the same fact); no other
    // word does, so a class never stands in for a verdict.
    let shared: Vec<&&str> = verdicts.iter().filter(|w| classes.contains(w)).collect();
    assert_eq!(shared, [&"exited"]);
    assert_eq!(Verdict::from_word("running"), None);
    assert_eq!(Class::from_word("stuck"), None);
}

// One block, one job (§11.3)

#[test]
fn every_command_of_a_block_is_one_job_with_one_id() {
    let mut w = World::new();
    let mut make = sleeper(1000, "make");
    make.cpu_per_s = 900_000_000;
    w.run_fg(1, 51, "make && make test", make);
    let _ = run_to(&mut w, 6_000, |w, _| w.line(1));
    // `make` exits 0; the shell holds the terminal for a tick…
    w.exit(1000, false);
    w.tab(1).tpgid = 200;
    let between = w.tick(TICK_MS);
    // …then puts `make test` in front, a group of its own.
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
    // `D`: the block's one job exits with the block's status; the shell is
    // never left behind as an orphan.
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

#[test]
fn fg_makes_a_parked_job_the_next_blocks_foreground_job_again() {
    let mut w = World::new();
    w.run_fg(1, 12, "vim notes", sleeper(1200, "vim"));
    w.tty_mode(1, false, false);
    let _ = run_to(&mut w, 4_000, |_, _| {});
    w.p(1200).state = ProcState::Stopped;
    w.close_block(1, 148);
    let v = run_to(&mut w, 10_000, |_, _| {});
    assert_eq!(v.row("b12").map(|r| r.verdict), Some(Verdict::Parked));
    // `fg`: block 13 opens and puts the stopped group back in front.
    let at = w.at;
    w.tab(1).tab.block = Some(Block {
        id: 13,
        started_ms: at,
        open: true,
        command: "fg".to_owned(),
        status: None,
    });
    w.tab(1).tpgid = 1200;
    w.p(1200).state = ProcState::Sleeping;
    w.tty_mode(1, false, false);
    let v = w.tick(TICK_MS);
    assert!(v.row("b13").is_none(), "{:?}", v.rows);
    let row = v.row("b12").expect("the resumed job keeps its id");
    assert_eq!(
        (row.kind, row.consumer),
        (JobKind::Shell, Consumer::PersonFg)
    );
    assert_eq!(row.wait.as_ref().map(|w| w.name), Some(WaitName::TtyRaw));
    // vim exits: `D` for block 13 closes the job it resumed.
    w.exit(1200, false);
    w.close_block(1, 0);
    let v = w.tick(TICK_MS);
    assert_eq!(
        v.row("b12").map(|r| (r.verdict, r.exit)),
        Some((Verdict::Exited, Some(0)))
    );
}

// Naming the wait: a connection is the wait only when nothing else is

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
    // A kqueue whose only entry is a read on the silent connection: tcp.
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
    // The same connection beside a timer entry: the queue waits for more than
    // the connection, so the wait is an event loop (capped at quiet).
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
    // The same program in a canonical terminal (no tty name applies) polls.
    let mut w = World::new();
    let mut p = sleeper(1000, "poller");
    p.syscalls_per_s = 1_100;
    w.run_fg(1, 51, "./poller", p);
    let v = run_to(&mut w, 60_000, |_, _| {});
    assert_eq!(v.row("b51").map(|r| r.class), Some(Class::Polling));
}

/// A FULL-SCREEN PROGRAM AT ITS PROMPT THAT WRITES FILES (agy 1.4.0 at rest
/// appends to its own log every 5 s while its telemetry is refused): it
/// makes progress the terminal does not show, so it reads `running`, but it
/// holds the foreground at the person's raw-terminal read with no key
/// queued, so job demand parks (`Idle`: nothing more until its next
/// terminal output, the design's "a session parked at a raw-mode prompt: 0
/// job samples until its next PTY output"). NEGATIVE CONTROLS: the same
/// writer in a canonical terminal, or one that drew past its mode word, is
/// watched fast; so is a program at the raw read spinning a core with no
/// progress.
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

/// THE PARKED TABS (`JobsView::parked_tabs`, design §6.1): a tab is parked
/// exactly while every live job of it holds the foreground at the person's
/// own raw read and adds no demand, whatever its class: an idle full-screen
/// program at its prompt, or one at the prompt that writes its own files.
/// NEGATIVE CONTROLS, each leaving its tab unparked: the same program in a
/// canonical terminal (no read is named), one that drew past its mode word,
/// one spinning a core at the raw read (still watched fast), the raw program
/// beside a background job of the same tab, and an exited job alone (no live
/// job: nothing to park). Another tab's park never parks this one.
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

/// A full-screen program at its prompt whose fds 1 and 2 are its own log
/// (agy 1.4.5 in the real-Claude lab, 2026-10-21: the terminal raw, a log
/// line every 5 s while its telemetry post is refused), playing tab `key`'s
/// block 51 in `w`, the log growing a line a tick.
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

/// THE SAME PERSON'S READ BY THE OUTPUT-FILE CHANNEL (agy as `lsof` read it
/// in the real-Claude lab, 2026-10-21: fds 1 and 2 on its own log, a line
/// every 5 s): the log is the job's progress, so it reads `running`, and the
/// person's raw read still parks job demand. NEGATIVE CONTROLS: the same log
/// under a canonical word (a plain command with its output sent to a file)
/// is watched fast, and so is the raw program once it printed after its
/// word: one redraw at 2 s voids the word, and from then on only the log
/// grows, so nothing but the stale word could park it.
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

// The foreign-text rule (§13.3)

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
    // An agent's task id is a join key validated the same way.
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
    // `p_comm` keeps 16 bytes; the executable's own name is longer.
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
