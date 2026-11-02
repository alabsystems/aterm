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
        agent_since: None,
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
        agent_since: None,
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
        agent_since: Some(1200),
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
        agent_since: Some(1200),
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

/// A view offering a closed tab's or a dead run's row is read again on the slow cadence; an
/// orphan of a live tab does not ask for it.
#[test]
fn a_view_offering_a_gone_owners_row_asks_for_its_next_reading() {
    let gone = judged(
        &naming(orphan(2606, 1500), "s-closed"),
        &[],
        &[closed_tab()],
    )
    .unwrap();
    let live = judged(&naming(orphan(2607, 1500), "s-live"), &[live_tab()], &[]).unwrap();
    assert!(holds_an_offer(&LeftView {
        rows: vec![gone, live.clone()],
        ..LeftView::default()
    }));
    assert!(!holds_an_offer(&LeftView {
        rows: vec![live],
        ..LeftView::default()
    }));
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
        agent_since: Some(1200),
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
        agent_since: Some(1200),
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
