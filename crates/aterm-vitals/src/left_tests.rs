// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The judge's table: one candidate's facts, shaped as the kernel reads them for a closed tab's
//! `nohup` survivor (leftover M12), an agent's call (M18, M20), a dead verify run's stage, and a
//! crashed instance's warm spare, against the live and closed tabs a reading holds.

use super::*;

/// launchd's pid version in these facts.
const LAUNCHD: i32 = 1;
/// The launcher version every orphan here was started by: not launchd's.
const SHELL_VERSION: i32 = 7;

/// A closed tab's shell was born at unique id 1000; the reading that missed the tab saw 2000.
fn closed_tab() -> ClosedTab {
    ClosedTab {
        key: 2,
        sid: "s-closed".into(),
        label: "Tab 2".into(),
        number: Some(2),
        shell_uniqueid: 1000,
        horizon: 2000,
        closed_ms: 1_792_000_000_000,
        agent_spans: Vec::new(),
        left: 0,
    }
}

/// A live tab whose shell was born at unique id 1000.
fn live_tab() -> LiveTab {
    LiveTab {
        key: 3,
        sid: "s-live".into(),
        shell_pid: 300,
        shell_uniqueid: Some(1000),
        fg_pgid: Some(310),
        agent_spans: Vec::new(),
    }
}

/// An orphan launchd adopted, born at `uniqueid` into group 2600 with no terminal, whose thread
/// ran, with a token, running `/usr/bin/ugrep`.
fn orphan(pid: u32, uniqueid: u64) -> Facts {
    Facts {
        pid,
        ppid: 1,
        lineage: Some(Lin {
            uniqueid,
            parent: 1,
            pgid: 2600,
            launcher_version: Some(SHELL_VERSION),
            tty: None,
        }),
        token: Some(format!("{pid}@1792098840004410:331207")),
        exe: Some("/usr/bin/ugrep".into()),
        started_ms: Some(3_600_000),
        cpu_ms: Some(62),
        thread_ran: Some(true),
        listener: Some(false),
        sandboxed: Some(false),
        by_design: Some(false),
        ..Facts::default()
    }
}

/// `f` whose own environment names `sid`.
fn naming(mut f: Facts, sid: &str) -> Facts {
    f.marks = Some(Marks {
        withheld: false,
        session: Some(sid.into()),
        run: None,
    });
    f
}

/// Judges `f` against `live` and `closed`.
fn judged(f: &Facts, live: &[LiveTab], closed: &[ClosedTab]) -> Option<LeftRow> {
    judge(
        f,
        &Ctx {
            launchd: Some(LAUNCHD),
            live,
            closed,
            spared: &[100],
            tellers: &[],
        },
    )
}

/// `(state, why, tier, token)` of a row, for one assertion a case.
fn verdict(row: &LeftRow) -> (State, Why, Tier, bool) {
    (row.state, row.why, row.tier, row.token.is_some())
}

/// A dead run's record tie: `group`, `run` and `tree` as given, its launcher dead.
fn dead_run(group: bool, run: bool, tree: bool, auto: bool) -> RunTie {
    RunTie {
        id: "41983-1792163800331520".into(),
        group,
        run,
        tree,
        dead: true,
        auto,
    }
}

/// A closed tab's survivor whose executable did not read could be an aterm window as well as
/// any other program, and `end` has no executable to hold it to: its session's tie names it and
/// offers no token. NEGATIVE CONTROL: take an unread executable for no aterm program, and it is
/// offered.
#[test]
fn a_survivor_whose_executable_did_not_read_is_named_with_no_token() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.exe = None;
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Name, false)
    );
    assert_eq!(row.from, Source::Closed(2));
}

#[test]
fn a_nohup_survivor_of_a_closed_tab_is_owner_gone_and_offered() {
    let row = judged(
        &naming(orphan(2606, 1500), "s-closed"),
        &[],
        &[closed_tab()],
    )
    .unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Offer, true)
    );
    assert_eq!(row.proof, vec![Proof::Env, Proof::Time]);
    assert_eq!(row.from, Source::Closed(2));
    assert_eq!(row.token.as_deref(), Some("2606@1792098840004410:331207"));
    assert_eq!(row.exe_judged.as_deref(), Some("/usr/bin/ugrep"));
}

/// The closed tab's window shuts at its horizon: a process born after the reading that missed
/// the tab was not born while it lived, whatever its environment says. NEGATIVE CONTROL: drop
/// the horizon bound, and it reads as the closed tab's.
#[test]
fn a_process_born_after_the_close_is_not_the_closed_tabs() {
    let row = judged(
        &naming(orphan(2606, 2001), "s-closed"),
        &[],
        &[closed_tab()],
    )
    .unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Name, false)
    );
    assert_eq!(row.from, Source::Unknown);
    let at = judged(
        &naming(orphan(2606, 2000), "s-closed"),
        &[],
        &[closed_tab()],
    )
    .unwrap();
    assert_eq!(at.from, Source::Closed(2), "inclusive at the horizon");
}

/// The window opens after the shell: a process older than the tab's shell is not the session's.
/// NEGATIVE CONTROL: drop the shell floor, and it reads as the closed tab's.
#[test]
fn a_process_born_before_the_shell_is_not_the_sessions() {
    for uniqueid in [999, 1000] {
        let row = judged(
            &naming(orphan(2606, uniqueid), "s-closed"),
            &[],
            &[closed_tab()],
        )
        .unwrap();
        assert_eq!(row.from, Source::Unknown, "{uniqueid}");
        assert_eq!(row.tier, Tier::Name, "{uniqueid}");
    }
}

#[test]
fn a_live_tabs_orphan_is_offered_as_orphaned() {
    let row = judged(&naming(orphan(2606, 1500), "s-live"), &[live_tab()], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Offer, true)
    );
    assert_eq!(row.from, Source::Tab(3));
}

/// An environment the kernel withheld proves no session, even one that carried a word: it is
/// no proof either way. NEGATIVE CONTROL: read a withheld environment as a match, and it is
/// offered as the closed tab's.
#[test]
fn a_withheld_environment_proves_no_session() {
    let mut f = orphan(2606, 1500);
    f.marks = Some(Marks {
        withheld: true,
        session: Some("s-closed".into()),
        run: None,
    });
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Name, false)
    );
    assert_eq!(row.proof, Vec::<Proof>::new());
}

/// A program the check holds gets no token whatever ties it (O4): its own environment is never
/// read, and a group-mate's still ties it to the closed tab. NEGATIVE CONTROL: drop O4, and the
/// held row is offered.
#[test]
fn a_held_row_gets_no_token_whatever_its_witness() {
    let mut f = orphan(2606, 1500);
    f.thread_ran = Some(false);
    f.held = Some(Held {
        check_ms: 61_000,
        tab: None,
    });
    f.mate_marks = Some(Marks {
        withheld: false,
        session: Some("s-closed".into()),
        run: None,
    });
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::ProgramCheck, Why::OwnerGone, Tier::Name, false)
    );
    assert_eq!(row.check_ms, Some(61_000));
    assert_eq!(row.from, Source::Closed(2));
    let mut unread = naming(orphan(2606, 1500), "s-closed");
    unread.thread_ran = None;
    assert_eq!(
        judged(&unread, &[], &[closed_tab()]).unwrap().tier,
        Tier::Name,
        "a thread list that did not read is no proof it ran"
    );
}

/// A closed tab's survivor that holds a listening socket is daemon-shaped (a `codex
/// app-server`), and named. NEGATIVE CONTROL: drop the listener clause, and it is offered.
#[test]
fn a_listener_left_by_a_closed_tab_is_daemon_shaped_and_named() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.listener = Some(true);
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Daemon, Tier::Name, false));
    assert!(row.listen);
}

/// A listener the closed tab's agent started (born after its first call) is that agent's dev
/// server: offered, not daemon-shaped. One born before the agent started stays daemon-shaped.
/// NEGATIVE CONTROL: drop the agent's exception from the listener clause, and the dev server is
/// named.
#[test]
fn an_agents_listener_born_after_its_agent_started_is_offered() {
    let tab = ClosedTab {
        agent_spans: vec![AgentSpan {
            since: 1200,
            until: None,
        }],
        ..closed_tab()
    };
    let mut server = naming(orphan(2606, 1500), "s-closed");
    server.listener = Some(true);
    let row = judged(&server, &[], std::slice::from_ref(&tab)).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Offer, true)
    );
    assert!(row.listen);
    let mut before = naming(orphan(2607, 1100), "s-closed");
    before.listener = Some(true);
    let row = judged(&before, &[], &[tab]).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Daemon, Tier::Name, false));
}

/// `trustd`'s shape: it leads its own group, has no terminal, and runs from outside every
/// recorded root. Daemon-shaped with no name consulted, and so even when an agent started it.
/// NEGATIVE CONTROL: drop the leader clause, and it is offered.
#[test]
fn a_group_leader_with_no_tty_outside_every_root_is_daemon_shaped() {
    let mut f = naming(orphan(2606, 1500), "s-live");
    if let Some(l) = f.lineage.as_mut() {
        l.pgid = 2606;
    }
    f.exe = Some("/Users//u/.atpkg/bin/trustd".into());
    let tab = LiveTab {
        agent_spans: vec![AgentSpan {
            since: 1200,
            until: None,
        }],
        ..live_tab()
    };
    let row = judged(&f, &[tab], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Daemon, Tier::Name, false)
    );
}

#[test]
fn a_daemon_shaped_member_under_a_dead_runs_roots_is_still_offered() {
    let mut f = orphan(2606, 1500);
    f.listener = Some(true);
    f.exe_under_root = true;
    f.runs = vec![dead_run(true, false, true, false)];
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::LauncherDead, Tier::Offer, true)
    );
    assert_eq!(row.proof, vec![Proof::Group, Proof::Tree, Proof::Time]);
}

#[test]
fn a_dead_runs_member_running_its_build_is_auto() {
    let mut f = orphan(2606, 1500);
    f.exe_under_root = true;
    f.runs = vec![dead_run(true, true, true, true)];
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::LauncherDead, Tier::Auto, true)
    );
    assert_eq!(
        row.proof,
        vec![Proof::Group, Proof::Run, Proof::Tree, Proof::Time]
    );
    assert_eq!(row.from, Source::Run("41983-1792163800331520".into()));
}

/// A dead run's shell outside its roots (a warm spare a test under verify started) carries the
/// run's id, so it is the run's, but runs no build of the run's: named with its run proof.
/// NEGATIVE CONTROL: offer W-run without C-tree, and it is offered.
#[test]
fn a_dead_runs_shell_outside_its_roots_is_named_with_its_run_proof() {
    let mut f = orphan(2606, 1500);
    f.exe = Some("/bin/zsh".into());
    f.runs = vec![dead_run(false, true, false, false)];
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::LauncherDead, Tier::Name, false)
    );
    assert_eq!(row.proof, vec![Proof::Run, Proof::Time]);
}

/// A live run ends its own. NEGATIVE CONTROL: drop the claims check, and its member is offered.
#[test]
fn a_live_runs_member_is_never() {
    let mut f = orphan(2606, 1500);
    f.exe_under_root = true;
    f.claimed = true;
    f.runs = vec![RunTie {
        dead: false,
        ..dead_run(true, true, true, false)
    }];
    let row = judged(&naming(f, "s-closed"), &[], &[closed_tab()]).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Live, Tier::Never, false));
}

#[test]
fn a_live_sessions_foreground_group_is_never() {
    let mut f = naming(orphan(2606, 1500), "s-live");
    if let Some(l) = f.lineage.as_mut() {
        l.pgid = 310;
    }
    let row = judged(&f, &[live_tab()], &[]).unwrap();
    assert_eq!(row.tier, Tier::Never);
    assert_eq!(row.token, None);
}

#[test]
fn a_tty_whose_master_lives_is_never() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    if let Some(l) = f.lineage.as_mut() {
        l.tty = Some(0x1000_0005);
    }
    f.tty_master_held = Some(true);
    assert_eq!(judged(&f, &[], &[closed_tab()]).unwrap().tier, Tier::Never);
    f.tty_master_held = None;
    assert_eq!(
        judged(&f, &[], &[closed_tab()]).unwrap().tier,
        Tier::Name,
        "an unread terminal scan offers nothing"
    );
    f.tty_master_held = Some(false);
    assert_eq!(
        judged(&f, &[], &[closed_tab()]).unwrap().tier,
        Tier::Offer,
        "its master closed with its tab"
    );
}

/// An orphaned instance proven its entry's writer is offered by W-writer where it holds no
/// listener; one holding its control socket's listener, as a live instance does, is
/// daemon-shaped (O3) and named.
#[test]
fn an_orphaned_instance_proven_its_entrys_writer_is_offered() {
    let mut f = orphan(2606, 1500);
    f.exe = Some("/Applications/aterm.app/Contents/MacOS/aterm".into());
    f.aterm = true;
    f.writer = true;
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Offer, true)
    );
    assert_eq!(row.proof, vec![Proof::Writer]);
    f.listener = Some(true);
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Daemon, Tier::Name, false)
    );
}

/// An instance whose launcher lives is no orphan, so it is no leftover; one the check holds is
/// listed, and never ended from here.
#[test]
fn an_instance_whose_launcher_lives_is_never() {
    let mut f = orphan(2606, 1500);
    f.ppid = 500;
    if let Some(l) = f.lineage.as_mut() {
        l.parent = 500;
    }
    f.aterm = true;
    f.writer = true;
    assert_eq!(judged(&f, &[], &[]), None);
    f.held = Some(Held::default());
    f.thread_ran = Some(false);
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::ProgramCheck, Why::Live, Tier::Never, false)
    );
}

/// A tether's watcher carries its child's environment, so W-session would tie it; ending it
/// would untie its child. It is never ended from here, and neither is a build's claim holder.
/// NEGATIVE CONTROL: leave a program launchd keeps by design out of NEVER, and the watcher of
/// the closed tab's helper is offered.
#[test]
fn a_tether_watcher_or_a_claim_holder_is_never() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.exe = Some("/Applications/aterm.app/Contents/MacOS/aterm".into());
    f.aterm = true;
    f.by_design = Some(true);
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Live, Tier::Never, false));
}

#[test]
fn an_unreadable_lineage_is_named_never_offered() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.lineage = None;
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Name, false)
    );
    assert_eq!(row.proof, Vec::<Proof>::new());
}

/// A crashed instance's warm spare, read by another instance: an orphaned shell whose session
/// id this instance never hosted (unknown, not gone) and whose terminal's master closed with its
/// instance. No witness ties it, and it is listed all the same, by name. NEGATIVE CONTROL: leave
/// an orphan no witness ties out of the view, and the spare is not listed.
#[test]
fn an_orphan_no_witness_ties_is_listed_and_named() {
    let mut f = naming(orphan(2606, 1500), "s-of-a-crashed-instance");
    f.exe = Some("/bin/zsh".into());
    if let Some(l) = f.lineage.as_mut() {
        l.pgid = 2606;
        l.tty = Some(0x1000_0007);
    }
    f.tty_master_held = Some(false);
    let row = judged(&f, &[live_tab()], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Name, false)
    );
    assert_eq!(row.from, Source::Unknown);
}

/// A spared pid (this instance, its ancestors) is never ended from here.
#[test]
fn a_spared_pid_is_never() {
    let f = naming(orphan(100, 1500), "s-closed");
    assert_eq!(judged(&f, &[], &[closed_tab()]).unwrap().tier, Tier::Never);
}

/// A row an agent's call of a live session left carries that session as its teller; a
/// person's `nohup` survivor (an orphan of a shell job) and a closed tab's carry none.
/// NEGATIVE CONTROL: name a teller for every orphan unit, and the shell job's survivor gets one.
#[test]
fn an_agent_calls_orphan_names_its_live_session_as_teller() {
    use crate::jobs::{JobRow, JobsView};
    let row = |pid: u32, tab: u64, of: JobKind| JobRow {
        kind: JobKind::Orphan,
        orphan_of: Some(of),
        tab,
        leader: (pid, Some(1)),
        ..crate::sys::jobs_view::fixtures::row("j1", 3, crate::jobs::Class::Waiting, Verdict::Fine)
    };
    let view = JobsView {
        rows: vec![
            row(2606, 3, JobKind::AgentCall),
            row(2607, 3, JobKind::Shell),
            row(2608, 2, JobKind::AgentCall),
        ],
        ..JobsView::default()
    };
    assert_eq!(tellers(&view, &[live_tab()]), vec![(2606, "s-live".into())]);
    let ctx = Ctx {
        launchd: Some(LAUNCHD),
        live: &[live_tab()],
        closed: &[],
        spared: &[],
        tellers: &[(2606, "s-live".into())],
    };
    let told = judge(&naming(orphan(2606, 1500), "s-live"), &ctx).unwrap();
    assert_eq!(told.teller.as_deref(), Some("s-live"));
}

#[test]
fn a_persons_nohup_survivor_has_no_teller() {
    use crate::jobs::{JobRow, JobsView};
    let view = JobsView {
        rows: vec![JobRow {
            kind: JobKind::Orphan,
            orphan_of: Some(JobKind::Shell),
            tab: 3,
            leader: (2607, Some(1)),
            ..crate::sys::jobs_view::fixtures::row(
                "j1",
                3,
                crate::jobs::Class::Waiting,
                Verdict::Fine,
            )
        }],
        ..JobsView::default()
    };
    assert!(tellers(&view, &[live_tab()]).is_empty());
    let row = judged(&naming(orphan(2607, 1500), "s-live"), &[live_tab()], &[]).unwrap();
    assert_eq!(row.teller, None);
}

/// A live job whose wait is another member's program check is listed waiting behind it, and so
/// is a process whose pipe's writer is held; the held member itself is the held row.
#[test]
fn a_job_waiting_behind_a_held_member_is_listed_waiting() {
    use crate::jobs::{JobRow, JobsView, Named, Standing, Wait};
    let held = Named {
        pid: 4001,
        exec_version: None,
        name: "git".into(),
        tab: Some(3),
        job: None,
        for_ms: 30_000,
    };
    let view = JobsView {
        rows: vec![
            JobRow {
                tab: 3,
                leader: (4000, Some(1)),
                wait: Some(Wait::bare(
                    WaitName::ProgramCheck,
                    Standing::Legit,
                    4001,
                    "git",
                )),
                ..crate::sys::jobs_view::fixtures::row(
                    "j1",
                    3,
                    crate::jobs::Class::Waiting,
                    Verdict::Fine,
                )
            },
            JobRow {
                tab: 3,
                leader: (4010, Some(1)),
                wait: Some(Wait {
                    peer: Some(4001),
                    ..Wait::bare(WaitName::Pipe, Standing::Legit, 4011, "ugrep")
                }),
                ..crate::sys::jobs_view::fixtures::row(
                    "j1",
                    3,
                    crate::jobs::Class::Waiting,
                    Verdict::Fine,
                )
            },
        ],
        findings: crate::jobs::Findings {
            program_check: vec![held],
            ..crate::jobs::Findings::default()
        },
        ..JobsView::default()
    };
    assert_eq!(
        waiting_of(&view),
        vec![
            (
                4000,
                Waiting {
                    peer: Some(4001),
                    tab: 3
                }
            ),
            (
                4011,
                Waiting {
                    peer: Some(4001),
                    tab: 3
                }
            ),
        ]
    );
    assert_eq!(
        held_of(&view, 0),
        vec![(
            4001,
            Held {
                check_ms: 30_000,
                tab: Some(3)
            }
        )]
    );
    assert_eq!(
        held_of(&view, 25_000)[0].1.check_ms,
        55_000,
        "a view the reading carries from a pass 25 s before is told on to the reading"
    );
    let mut f = orphan(4000, 1500);
    f.ppid = 300;
    if let Some(l) = f.lineage.as_mut() {
        l.parent = 300;
    }
    f.waiting = Some(Waiting {
        peer: Some(4001),
        tab: 3,
    });
    let row = judged(&f, &[live_tab()], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Waiting, Why::Live, Tier::Name, false)
    );
    assert_eq!((row.peer, row.from), (Some(4001), Source::Tab(3)));
}

/// A view offering a closed tab's or a dead run's row is read again on the slow cadence, and so
/// is one naming an agent's left row of a live tab, whose end no edge of its session tells; the
/// person's orphan of a live tab, born before its agent started, does not ask for it. NEGATIVE
/// CONTROL: ask for gone owners' rows alone, and the agent's row is never read again.
#[test]
fn a_view_offering_a_gone_owners_row_asks_for_its_next_reading() {
    let gone = judged(
        &naming(orphan(2606, 1500), "s-closed"),
        &[],
        &[closed_tab()],
    )
    .unwrap();
    let tabs = [agent_tab()];
    let person = judged(&naming(orphan(2607, 1100), "s-live"), &tabs, &[]).unwrap();
    let agents = judged(&naming(orphan(2608, 1500), "s-live"), &tabs, &[]).unwrap();
    let view = |rows: Vec<LeftRow>| LeftView {
        rows,
        live: tabs.to_vec(),
        ..LeftView::default()
    };
    assert!(holds_an_offer(&view(vec![gone, person.clone()])));
    assert!(!holds_an_offer(&view(vec![person.clone()])));
    assert!(holds_an_offer(&view(vec![person, agents])));
}

/// The daemon shape is waived for a dead run's member its witness ties (W-group or W-run) that
/// runs the run's build; an executable that only sits under a dead run's roots, tied to no run,
/// waives nothing. NEGATIVE CONTROL: waive the shape for any executable under a dead run's
/// roots, and a closed tab's listener built in that worktree is offered.
#[test]
fn a_dead_runs_roots_alone_waive_no_daemon_shape() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.listener = Some(true);
    f.exe_under_root = true;
    f.runs = vec![dead_run(false, false, true, false)];
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Daemon, Tier::Name, false));
    assert_eq!(row.from, Source::Closed(2));
}

/// A row two dead runs tie is the run's whose verdict made it AUTO, so that run's row counts it
/// and says the next verify run ends it. NEGATIVE CONTROL: take the first dead run that ties
/// it, and the row is the other run's.
#[test]
fn an_auto_row_is_the_run_whose_verdict_made_it_so() {
    let mut f = orphan(2606, 1500);
    f.exe_under_root = true;
    f.runs = vec![
        RunTie {
            id: "500-1792000000000500".into(),
            ..dead_run(true, false, true, false)
        },
        dead_run(false, true, true, true),
    ];
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(row.tier, Tier::Auto);
    assert_eq!(row.from, Source::Run("41983-1792163800331520".into()));
}

/// Where the kernel keeps no launcher version (Linux), a session's orphan is one init adopted:
/// a row whose parent is another process is no session's, whatever its environment names.
/// NEGATIVE CONTROL: read any parent as an adopter, and the row is the closed tab's.
#[test]
fn a_row_with_a_live_parent_is_no_sessions_where_no_launcher_version_is_kept() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    if let Some(l) = f.lineage.as_mut() {
        l.launcher_version = None;
    }
    let adopted = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(adopted.from, Source::Closed(2), "init adopted it");
    f.ppid = 1200;
    if let Some(l) = f.lineage.as_mut() {
        l.parent = 1200;
    }
    assert_eq!(judged(&f, &[], &[closed_tab()]), None);
}

/// A listener table that did not read rules no listener out: a closed tab's survivor is named,
/// never offered, unless it is its agent's own, whose listener the shape waives anyway.
/// NEGATIVE CONTROL: read an unread table as no listener, and the survivor is offered.
#[test]
fn an_unread_listener_table_offers_no_token() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.listener = None;
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Name, false)
    );
    assert!(!row.listen);
    let tab = ClosedTab {
        agent_spans: vec![AgentSpan {
            since: 1200,
            until: None,
        }],
        ..closed_tab()
    };
    assert_eq!(judged(&f, &[], &[tab]).unwrap().tier, Tier::Offer);
}

/// An aterm program's listener is its control socket or a broker's, never an agent's dev
/// server: a window an agent started in the background, adopted when its tool shell exited, is
/// daemon-shaped and named. One started with no control socket holds no listener, and W-session
/// alone still offers it no token. NEGATIVE CONTROL: waive the listener for an aterm program the
/// agent started, and the window is offered; let W-session alone offer an aterm program, and
/// the socketless window is.
#[test]
fn an_aterm_programs_listener_is_no_agents_dev_server() {
    let tab = ClosedTab {
        agent_spans: vec![AgentSpan {
            since: 1200,
            until: None,
        }],
        ..closed_tab()
    };
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.exe = Some("/Users//u/src/wt/target/debug/aterm".into());
    f.aterm = true;
    f.writer = true;
    f.listener = Some(true);
    let row = judged(&f, &[], std::slice::from_ref(&tab)).unwrap();
    assert_eq!(verdict(&row), (State::Left, Why::Daemon, Tier::Name, false));
    f.writer = false;
    f.listener = Some(false);
    let row = judged(&f, &[], &[tab]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Name, false),
        "a window with no control socket"
    );
}

/// A process a sandbox profile holds is named with the reason it is listed: a lab's jail member
/// and an agent's sandboxed tool command read alike here, so neither is offered, and neither is
/// read as live. A failed check is no proof it is not held. NEGATIVE CONTROL: offer a sandboxed
/// row, and the closed tab's sandboxed survivor gets a token.
#[test]
fn a_sandboxed_row_is_named_with_its_reason() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.sandboxed = Some(true);
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Name, false)
    );
    f.sandboxed = None;
    assert_eq!(
        judged(&f, &[], &[closed_tab()]).unwrap().tier,
        Tier::Name,
        "an unread check"
    );
}

/// An aterm image whose verb did not read may be a live tether watcher: it is named, never
/// offered, even where its witness (here W-writer) would offer it. NEGATIVE CONTROL: offer an
/// aterm image whose verb is unread, and it gets a token.
#[test]
fn an_aterm_image_whose_verb_did_not_read_is_named() {
    let mut f = orphan(2606, 1500);
    f.exe = Some("/Applications/aterm.app/Contents/MacOS/aterm".into());
    f.aterm = true;
    f.writer = true;
    f.by_design = None;
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Name, false)
    );
}

/// The view a reading `gone_ms` after `closed_tab()` closed makes of `rows`, with a dead run
/// whose launcher was found dead `gone_ms` ago and the build directories `build_dirs` the disk
/// watch keeps.
fn entry_view(rows: Vec<LeftRow>, gone_ms: u64, build_dirs: &[&str]) -> LeftView {
    let tab = closed_tab();
    LeftView {
        rows,
        runs: vec![RunRow {
            id: "41983-1792163800331520".into(),
            dead: true,
            dead_ms: Some(gone_ms),
            sid: Some("s-closed".into()),
            roots: vec!["/w/wt".into()],
            left: 1,
            next_verify: false,
        }],
        as_of_ms: tab.closed_ms + gone_ms,
        closed: vec![tab],
        build_dirs: build_dirs.iter().map(|d| (*d).to_owned()).collect(),
        ..LeftView::default()
    }
}

/// A closed tab's `nohup` survivor, offered, that has run `age_ms` and spent `cpu_ms` of CPU.
fn survivor(age_ms: u64, cpu_ms: u64) -> LeftRow {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.started_ms = Some(age_ms);
    f.cpu_ms = Some(cpu_ms);
    let row = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::OwnerGone, Tier::Offer, true)
    );
    row
}

/// A dead verify run's member running its build, idle: its launcher is gone, and it costs nothing.
fn dead_runs_member() -> LeftRow {
    let mut f = orphan(4300, 1500);
    f.exe = Some("/w/wt/target/debug/aterm-link".into());
    f.exe_under_root = true;
    f.cpu_ms = Some(0);
    f.runs = vec![dead_run(true, false, true, false)];
    let row = judged(&f, &[], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Left, Why::LauncherDead, Tier::Offer, true)
    );
    row
}

/// A dead run's leftover is aterm's own leak: the Entry names it once its launcher has been dead
/// an hour, though it costs nothing. NEGATIVE CONTROL: hold a dead run's row to the cost a closed
/// tab's must meet, and the idle member is not named.
#[test]
fn a_dead_runs_leftover_an_hour_old_is_eligible_even_idle() {
    let row = dead_runs_member();
    let view = entry_view(vec![row.clone()], ENTRY_AFTER_MS, &[]);
    assert!(!costs(&row, &view), "idle, and no build directory is kept");
    assert_eq!(owner_gone_ms(&row, &view), Some(ENTRY_AFTER_MS));
    assert!(eligible(&row, &view, ENTRY_AFTER_MS));
}

/// A closed tab's survivor that sleeps (62 ms of CPU in an hour) costs nothing: someone may have
/// `nohup`ed it on purpose, so the Entry leaves it to `sys left`.
#[test]
fn a_closed_tabs_idle_leftover_is_not_eligible() {
    let row = survivor(3_600_000, 62);
    let view = entry_view(vec![row.clone()], 2 * ENTRY_AFTER_MS, &[]);
    assert!(!eligible(&row, &view, ENTRY_AFTER_MS));
}

/// One second of CPU for each hour of its age is a cost: a survivor that has burned that much is
/// named, and one just under it is not.
#[test]
fn a_closed_tabs_leftover_burning_a_second_an_hour_is_eligible() {
    let burning = survivor(7_200_000, 2_000);
    let under = survivor(7_200_000, 1_999);
    let view = entry_view(vec![burning.clone(), under.clone()], ENTRY_AFTER_MS, &[]);
    assert!(eligible(&burning, &view, ENTRY_AFTER_MS));
    assert!(!eligible(&under, &view, ENTRY_AFTER_MS));
}

/// A survivor running a binary from a build directory the disk watch must keep costs disk, however
/// idle it is; one outside every kept directory does not.
#[test]
fn an_executable_under_a_kept_build_dir_makes_a_closed_tabs_leftover_eligible() {
    let mut row = survivor(3_600_000, 0);
    row.exe_judged = Some("/w/other/target/debug/server".into());
    let kept = entry_view(vec![row.clone()], ENTRY_AFTER_MS, &["/w/other/target"]);
    assert!(eligible(&row, &kept, ENTRY_AFTER_MS));
    let elsewhere = entry_view(vec![row.clone()], ENTRY_AFTER_MS, &["/w/other/targets"]);
    assert!(
        !eligible(&row, &elsewhere, ENTRY_AFTER_MS),
        "a directory's name prefix is not its tree"
    );
}

/// An orphan of a tab that still lives is never named, however much it burns: its owner is not
/// gone, so only `sys left` lists it. NEGATIVE CONTROL: read a live tab's row as gone since its
/// fork, and the burning orphan is named.
#[test]
fn an_orphan_of_a_live_tab_is_never_eligible() {
    let mut f = naming(orphan(2606, 1500), "s-live");
    f.started_ms = Some(7_200_000);
    f.cpu_ms = Some(7_000_000);
    let row = judged(&f, &[live_tab()], &[]).unwrap();
    assert_eq!(
        verdict(&row),
        (State::Orphaned, Why::Orphaned, Tier::Offer, true)
    );
    let view = entry_view(vec![row.clone()], 10 * ENTRY_AFTER_MS, &[]);
    assert!(!eligible(&row, &view, ENTRY_AFTER_MS));
    assert_eq!(owner_gone_ms(&row, &view), None);
}

/// Nothing is named before its owner has been gone an hour: not a dead run's member, not a
/// survivor that burns. NEGATIVE CONTROL: drop the age gate, and both are named at once.
#[test]
fn under_an_hour_nothing_is_eligible() {
    let member = dead_runs_member();
    let burning = survivor(7_200_000, 7_000_000);
    let view = entry_view(
        vec![member.clone(), burning.clone()],
        ENTRY_AFTER_MS - 1,
        &[],
    );
    assert!(!eligible(&member, &view, ENTRY_AFTER_MS));
    assert!(!eligible(&burning, &view, ENTRY_AFTER_MS));
    assert!(
        eligible(&member, &view, ENTRY_AFTER_MS - 1),
        "at its own bound"
    );
}

/// A row an end does not take is never named, however long its owner has been gone and however
/// much it burns: a closed tab's daemon (a listener, NAME with no token) and a row a live run
/// still claims (NEVER) are `sys left`'s to list, and the Entry's one step, ending each by its
/// token, would have none to name. NEGATIVE CONTROL: drop the tier gate, and the burning daemon
/// is named.
#[test]
fn a_row_an_end_does_not_take_is_never_eligible() {
    let mut f = naming(orphan(2606, 1500), "s-closed");
    f.started_ms = Some(7_200_000);
    f.cpu_ms = Some(7_000_000);
    f.listener = Some(true);
    let daemon = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&daemon),
        (State::Left, Why::Daemon, Tier::Name, false)
    );
    f.listener = Some(false);
    f.claimed = true;
    let claimed = judged(&f, &[], &[closed_tab()]).unwrap();
    assert_eq!(
        verdict(&claimed),
        (State::Left, Why::Live, Tier::Never, false)
    );
    let view = entry_view(
        vec![daemon.clone(), claimed.clone()],
        2 * ENTRY_AFTER_MS,
        &[],
    );
    for row in [&daemon, &claimed] {
        assert!(costs(row, &view), "it burns");
        assert_eq!(owner_gone_ms(row, &view), Some(2 * ENTRY_AFTER_MS));
        assert!(!eligible(row, &view, ENTRY_AFTER_MS), "{row:?}");
    }
}

/// A live tab whose agent started at unique id 1200.
fn agent_tab() -> LiveTab {
    LiveTab {
        agent_spans: vec![AgentSpan {
            since: 1200,
            until: None,
        }],
        ..live_tab()
    }
}

/// A row W-session ties to a live tab records whether it was born after that tab's agent
/// started: one born at 1100, after the shell and before the agent, is offered and reads false;
/// one born at 1500 reads true; one W-session ties to no session reads false. NEGATIVE CONTROL:
/// set the field from the tie alone, and the one born before the agent reads true.
#[test]
fn a_row_records_whether_it_was_born_after_its_agent() {
    let tabs = [agent_tab()];
    let before = judged(&naming(orphan(2606, 1100), "s-live"), &tabs, &[]).unwrap();
    let after = judged(&naming(orphan(2607, 1500), "s-live"), &tabs, &[]).unwrap();
    let untied = judged(&orphan(2608, 1500), &tabs, &[]).unwrap();
    for row in [&before, &after] {
        assert_eq!(
            (verdict(row), row.from.clone()),
            (
                (State::Orphaned, Why::Orphaned, Tier::Offer, true),
                Source::Tab(3)
            ),
            "{row:?}"
        );
    }
    assert_eq!(untied.from, Source::Unknown);
    assert_eq!(
        (before.after_agent, after.after_agent, untied.after_agent),
        (false, true, false)
    );
}

/// THE AGENT'S LEFT ROWS ARE ITS OFFERED SESSION ROWS AND ITS DEAD RUNS': of one view over an
/// agent's tab (`s-live`, its agent started at 1200) and a tab with no agent (`s-other`), the
/// agent's are the offered row its session's environment names born after its agent started and
/// the offered member of a dead verify run that rode in its session. Not its sandboxed row (named,
/// no token), the person's `nohup` from before the agent, the dead run's AUTO member (the next
/// verify run's to end), nor the other tab's row, which is that tab's own: born after its shell,
/// where no agent start is known. No session id names none. NEGATIVE CONTROL: drop the
/// after-agent filter, and the person's `nohup` counts as the agent's.
#[test]
fn the_agents_rows_are_its_offered_session_rows_and_its_dead_runs() {
    let other = LiveTab {
        key: 4,
        sid: "s-other".into(),
        shell_pid: 400,
        ..live_tab()
    };
    let tabs = [agent_tab(), other];
    let mut sandboxed = naming(orphan(2607, 1500), "s-live");
    sandboxed.sandboxed = Some(true);
    let mut auto = orphan(2609, 1500);
    auto.exe_under_root = true;
    auto.runs = vec![dead_run(true, true, true, true)];
    let mut dead = orphan(2611, 1500);
    dead.exe_under_root = true;
    dead.started_ms = Some(7_200_000);
    dead.runs = vec![dead_run(true, false, true, false)];
    let facts = [
        naming(orphan(2606, 1500), "s-live"),
        sandboxed,
        naming(orphan(2608, 1100), "s-live"),
        auto,
        naming(orphan(2610, 1500), "s-other"),
        dead,
    ];
    let rows: Vec<LeftRow> = facts
        .iter()
        .map(|f| judged(f, &tabs, &[]).unwrap())
        .collect();
    let tiers: Vec<(u32, Tier)> = rows.iter().map(|r| (r.pid, r.tier)).collect();
    assert_eq!(
        tiers,
        [
            (2606, Tier::Offer),
            (2607, Tier::Name),
            (2608, Tier::Offer),
            (2609, Tier::Auto),
            (2610, Tier::Offer),
            (2611, Tier::Offer),
        ]
    );
    let view = LeftView {
        rows,
        runs: vec![RunRow {
            id: "41983-1792163800331520".into(),
            dead: true,
            dead_ms: Some(60_000),
            sid: Some("s-live".into()),
            roots: vec!["/w/wt".into()],
            left: 2,
            next_verify: true,
        }],
        live: tabs.to_vec(),
        complete: true,
        ..LeftView::default()
    };
    let pids = |sid| -> Vec<u32> { agents_left(&view, sid).iter().map(|r| r.pid).collect() };
    assert_eq!(pids("s-live"), [2606, 2611]);
    assert_eq!(
        pids("s-other"),
        [2610],
        "a tab with no agent: after its shell"
    );
    assert!(pids("").is_empty() && pids("s-gone").is_empty());
    assert_eq!(
        agents_left_count(&view, "s-live"),
        Some((2, Some(7_200_000)))
    );
    assert_eq!(agents_left_count(&view, "s-gone"), Some((0, None)));
}

/// A PARTIAL VIEW COUNTS NOTHING: the same view whose sweep stopped at its cap, or whose reading
/// stopped at its bound of new candidates, gives no count for the agent's session, though it
/// names its row, since it cannot say none is left. NEGATIVE CONTROL: count a partial view, and
/// it reads as whole.
#[test]
fn a_partial_view_counts_no_agents_rows() {
    let tabs = [agent_tab()];
    let row = judged(&naming(orphan(2606, 1500), "s-live"), &tabs, &[]).unwrap();
    let whole = LeftView {
        rows: vec![row],
        live: tabs.to_vec(),
        complete: true,
        ..LeftView::default()
    };
    assert_eq!(
        agents_left_count(&whole, "s-live"),
        Some((1, Some(3_600_000)))
    );
    for partial in [
        LeftView {
            complete: false,
            ..whole.clone()
        },
        LeftView {
            bounded: true,
            ..whole.clone()
        },
    ] {
        assert_eq!(agents_left(&partial, "s-live").len(), 1);
        assert_eq!(agents_left_count(&partial, "s-live"), None, "{partial:?}");
    }
}

/// THE STRETCHES AN AGENT RAN IN A TAB: a named agent opens one; a reading that names an agent
/// whose id it did not read changes nothing; one that names none closes it at its horizon (one
/// whose horizon is below the start, having read no table, does not); the same agent named again
/// reopens it whole, and with an earlier start merges the stretches it now overlaps; a later agent
/// opens its own, and past [`AGENT_SPANS_KEPT`] the oldest two become one. A process born between
/// two stretches is in neither. NEGATIVE CONTROL: keep one stretch from the earliest start, and the
/// gap reads as the agent's.
#[test]
fn a_tabs_agent_stretches_open_close_and_merge() {
    let span = |since, until| AgentSpan { since, until };
    let mut spans = Vec::new();
    fold_agent(&mut spans, true, Some(1200), 1300);
    assert_eq!(spans, [span(1200, None)]);
    fold_agent(&mut spans, true, None, 1400);
    fold_agent(&mut spans, true, Some(1250), 1400);
    assert_eq!(spans, [span(1200, None)], "an unread id, a later call");
    fold_agent(&mut spans, false, None, 0);
    assert_eq!(spans, [span(1200, None)], "a reading that read no table");
    fold_agent(&mut spans, false, None, 1500);
    assert_eq!(spans, [span(1200, Some(1500))]);
    fold_agent(&mut spans, false, None, 1600);
    assert_eq!(spans, [span(1200, Some(1500))], "closed once");
    assert!(spans[0].holds(1500) && !spans[0].holds(1501) && !spans[0].holds(1199));
    fold_agent(&mut spans, true, Some(1200), 1700);
    assert_eq!(spans, [span(1200, None)], "the same agent named again");
    fold_agent(&mut spans, false, None, 1700);
    fold_agent(&mut spans, true, Some(2000), 2100);
    assert_eq!(spans, [span(1200, Some(1700)), span(2000, None)]);
    assert!(!spans.iter().any(|s| s.holds(1800)), "the person's gap");
    fold_agent(&mut spans, true, Some(1650), 2200);
    assert_eq!(spans, [span(1200, None)], "an earlier start merges");
    let mut many = Vec::new();
    for i in 0..=AGENT_SPANS_KEPT as u64 {
        fold_agent(&mut many, true, Some(10_000 + i * 100), 10_000 + i * 100);
        fold_agent(&mut many, false, None, 10_050 + i * 100);
    }
    assert_eq!(many.len(), AGENT_SPANS_KEPT);
    assert_eq!(many[0], span(10_000, Some(10_150)));
    assert!(!many.iter().any(|s| s.holds(10_175)));
}
