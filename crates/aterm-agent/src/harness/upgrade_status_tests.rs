// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::supervise::ScratchDir;
use std::path::PathBuf;

const NOW: u64 = 1_790_311_076;
const TAB: &str = "s-b5cf2faabac5ce5127bd";

fn opts(dir: &std::path::Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(dir.join("no.sock").to_string_lossy().into_owned()),
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: true,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        press: None,
        timings: crate::harness::Timings::FAST,
    }
}

fn holder(session: &str, version: &str, tab: Option<&str>) -> Holder {
    Holder {
        session: session.to_string(),
        version: version.to_string(),
        tab: tab.map(str::to_string),
        ..Holder::default()
    }
}

fn behind_in(tab: &'static str) -> impl Fn(&BTreeSet<String>) -> Option<Vec<Holder>> {
    move |sessions| {
        Some(
            sessions
                .iter()
                .map(|s| holder(s, "2.1.281", Some(tab)))
                .collect(),
        )
    }
}

fn pending(behind_for: u64) -> Row {
    Row {
        session: "03396a15-856e-4f1b-8174-ae9a3e4b369f".to_string(),
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        phase: Phase::Pending,
        behind_since: NOW - behind_for,
        wait: "not-idle:busy".to_string(),
        wait_since: NOW - behind_for + 60,
        ..Row::default()
    }
}

#[test]
fn only_an_upgrade_that_will_not_move_on_its_own_is_stalled() {
    assert_eq!(pending(3_600).stall(NOW), None, "an hour behind is healthy");
    assert_eq!(
        pending(STALLED_AFTER_S - 1).stall(NOW),
        None,
        "the bound is not yet reached"
    );
    assert_eq!(
        pending(STALLED_AFTER_S).stall(NOW).as_deref(),
        Some("overdue")
    );
    let tmux = Row {
        wait: "terminal:tmux".to_string(),
        ..pending(60)
    };
    assert_eq!(tmux.stall(NOW).as_deref(), Some("held-back:tmux"));
    for (why, stall) in [
        ("not-a-shell-job", "refused:not-a-shell-job"),
        ("argv:--print", "refused:argv:--print"),
        ("no-resume", "failed:no-resume"),
    ] {
        let stopped = Row {
            phase: Phase::Failed(why.to_string()),
            ..pending(60)
        };
        assert_eq!(stopped.stall(NOW).as_deref(), Some(stall), "{why}");
        assert!(stopped.stall_words(NOW).is_some());
    }
    for (behind, stall) in [(60, None), (46 * 3_600, Some("overdue"))] {
        let gave_up = Row {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            retry_at: NOW + 3_600,
            ..pending(behind)
        };
        assert_eq!(gave_up.stall(NOW).as_deref(), stall, "{behind}");
        assert_eq!(
            gave_up.stall_words(NOW).is_some(),
            stall.is_some(),
            "{behind}"
        );
        assert!(
            !gave_up.moving(NOW),
            "resting, it does not move at a turn end"
        );
    }
    let old = pending(9 * 3_600);
    let skipped = Row {
        request: Request::Skip("2.1.282".to_string()),
        ..old.clone()
    };
    assert_eq!(skipped.stall(NOW), None);
    let skipped_older = Row {
        request: Request::Skip("2.1.281".to_string()),
        ..old.clone()
    };
    assert_eq!(
        skipped_older.stall(NOW).as_deref(),
        Some("overdue"),
        "a skip of another version holds nothing"
    );
    assert!(
        skipped.word_in_force(NOW),
        "CONTROL: a skip of this build holds"
    );
    let revision = Row {
        agent: upgrade::Agent::Muse,
        from: "1.4.10-R6427.1".to_string(),
        to: "1.4.11-R6500.1".to_string(),
        request: Request::Skip("1.4.9-R6297.1".to_string()),
        ..old.clone()
    };
    assert!(
        !revision.word_in_force(NOW),
        "two revisions are not the same build"
    );
    let same_revision = Row {
        agent: upgrade::Agent::Muse,
        from: "1.4.10-R6427.1".to_string(),
        to: "1.4.10-R6500.1".to_string(),
        request: Request::Skip("1.4.10-R6500.1".to_string()),
        ..old.clone()
    };
    assert!(
        same_revision.word_in_force(NOW),
        "a skip of this very revision holds it"
    );
    assert_eq!(
        same_revision.stall(NOW),
        None,
        "a held revision is no stall"
    );
    for (a, b, same) in [
        ("1.4.10-R6500.1", "v1.4.10-R6500.1", true),
        ("1.4.10-R6500.1", "1.4.10-R6500.2", false),
        ("2.1.310", "2.1.310.0", true),
        ("0.165.0-dev.4+gabc", "0.165.0-dev.4+gabc", false),
        ("0.165.0-dev.4+gabc", "0.165.0-dev.5+gabc", false),
        ("", "", false),
    ] {
        assert_eq!(
            upgrade::same_build(upgrade::Agent::Muse, a, b),
            same,
            "{a:?} {b:?}"
        );
    }
    let deferred = Row {
        request: Request::DeferUntil(NOW + 60),
        ..old.clone()
    };
    assert_eq!(deferred.stall(NOW), None);
    assert_eq!(deferred.stall(NOW + 60).as_deref(), Some("overdue"));
    for phase in [
        Phase::Exiting { at_s: NOW },
        Phase::Relaunched { at_s: NOW },
        Phase::Done,
    ] {
        assert_eq!(
            Row {
                phase,
                ..old.clone()
            }
            .stall(NOW),
            None
        );
    }
}

#[test]
fn the_column_names_the_state_the_target_the_reason_and_the_age() {
    assert_eq!(
        pending(3_600).column(NOW),
        "pending/2.1.282/not-idle:busy/1h"
    );
    assert_eq!(
        pending(8 * 3_600 + 22 * 60).column(NOW),
        "stalled/2.1.282/overdue:not-idle:busy/8h22m"
    );
    let asked = Row {
        phase: Phase::Announced { at_s: NOW, asks: 1 },
        wait: "awaiting-ready".to_string(),
        ..pending(120)
    };
    assert_eq!(asked.column(NOW), "announced/2.1.282/awaiting-ready/2m");
    let deferred = Row {
        request: Request::DeferUntil(NOW + 60),
        ..pending(120)
    };
    assert_eq!(deferred.column(NOW), "deferred/2.1.282/defer:1m/2m");
    let skipped = Row {
        request: Request::Skip("2.1.282".to_string()),
        ..pending(120)
    };
    assert_eq!(skipped.column(NOW), "skipped/2.1.282/skip:2.1.282/2m");
    let restarting = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(120)
    };
    assert_eq!(restarting.column(NOW), "restarting/2.1.282/exiting/2m");
    let done = Row {
        phase: Phase::Done,
        done_at: NOW - 30,
        ..pending(120)
    };
    assert_eq!(done.column(NOW), "done/2.1.282/-/30s");
}

#[test]
fn a_word_fenced_to_a_gone_notices_tab_holds_neither_the_view_nor_the_drive() {
    for request in [
        Request::Skip("2.1.282".to_string()),
        Request::DeferUntil(NOW + 3_600),
    ] {
        for (holder, holds) in [("s-b", false), (TAB, true)] {
            let mut st = St {
                from: "2.1.281".to_string(),
                to: "2.1.282".to_string(),
                tab: TAB.to_string(),
                notice_pid: 4242,
                notice_start: "Fri Sep 25 16:00:00 2026".to_string(),
                request: request.clone(),
                request_tab: TAB.to_string(),
                request_at: NOW - 60,
                ..St::default()
            };
            st.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), NOW - 120, 1);
            let _ = st.notice_gone(holder, NOW);
            let drive = st.request_for(holder) != Request::None;
            let column = Row::of("c", &st, NOW).column(NOW);
            let view = column.starts_with("skipped/") || column.starts_with("deferred/");
            assert_eq!(
                (drive, view),
                (holds, holds),
                "{request:?} {holder}: {column}"
            );
        }
    }
}

#[test]
fn a_carried_on_restart_reads_restarting_while_its_model_is_waited_for() {
    let carried = Row {
        phase: Phase::Done,
        confirm_by: NOW + 60,
        ..pending(120)
    };
    assert_eq!(carried.column(NOW), "restarting/2.1.282/continued/2m");
    assert_eq!(carried.column(NOW + 60), "restarting/2.1.282/continued/3m");
    assert_eq!(carried.column(NOW + 61 + 40 * 60), "done/2.1.282/-/40m");
    let dir = ScratchDir::new("upgrade-status", "confirming");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let st = St {
        phase: Phase::Done,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now - 150,
        confirm_by: now + 120,
        resumed_on: "2.1.282".to_string(),
        ..St::default()
    };
    save(&o, "aaa", &st);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut view = View::default();
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let next = view.refresh_with(&o, &tabs, now, &held, &mut |rows| sent.push(rows.to_vec()));
    let row = &sent.last().expect("handed over")[0];
    assert_eq!(row.column(now), "restarting/2.1.282/continued/2m");
    assert_eq!(next, Some(now + 121), "the view looks again as it turns");
    view.refresh_with(&o, &tabs, now + 125, &held, &mut |rows| {
        sent.push(rows.to_vec())
    });
    assert_eq!(
        sent.last().expect("sent")[0].column(now + 125),
        "done/2.1.282/-/5s"
    );
    save(
        &o,
        "aaa",
        &St {
            confirm_by: 0,
            done_at: now + 130,
            ..st.clone()
        },
    );
    view.refresh_with(&o, &tabs, now + 135, &held, &mut |rows| {
        sent.push(rows.to_vec())
    });
    assert_eq!(
        sent.last().expect("sent")[0].column(now + 135),
        "done/2.1.282/-/5s"
    );
    save(
        &o,
        "aaa",
        &St {
            confirm_by: 0,
            ..st.clone()
        },
    );
    view.refresh_with(&o, &tabs, now + 136, &held, &mut |rows| {
        sent.push(rows.to_vec())
    });
    assert!(sent.last().expect("sent").is_empty(), "{sent:?}");
}

#[test]
fn status_rows_come_from_the_state_files_and_filter_by_tab() {
    let dir = ScratchDir::new("upgrade-status", "rows");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let mine = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 30_120,
        wait: "not-idle:busy".to_string(),
        wait_since: NOW - 30_000,
        ..St::default()
    };
    let other = St {
        tab: "s-4906566e7ab0a0c15ee3".to_string(),
        ..mine.clone()
    };
    save(&o, "aaa", &mine);
    save(&o, "bbb", &other);
    let all = rows_at(&o, NOW);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].session, "aaa", "sorted by conversation");
    let only = rows_at(
        &Opts {
            only_sid: Some(TAB.to_string()),
            ..o.clone()
        },
        NOW,
    );
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].tab, TAB);
    let json = only[0].to_json(NOW);
    assert_eq!(
        json.get("pending_for_s").and_then(Value::as_u64),
        Some(30_120)
    );
    assert_eq!(json.get("stalled").and_then(Value::as_str), Some("overdue"));
}

#[test]
fn an_afresh_moves_row_takes_a_word_and_names_its_marks_command() {
    const ID: &str = "0badf00d-1111-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "afresh-ask");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let stopped = |cause: &str, line: &str| St {
        phase: Phase::Failed("stale-exit".to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        exited_at: now_s() - 60,
        cause: cause.to_string(),
        line: line.to_string(),
        salt: 5,
        ..St::default()
    };
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    save(&o, ID, &stopped(fresh, ""));
    let e = ask(&o, TAB, Ask::Now, false).expect_err("not re-armed");
    assert!(e.contains("`claude` in the tab starts it again"), "{e}");
    assert!(!e.contains("--resume"), "{e}");
    let row = ask(&o, TAB, Ask::Skip, false).expect("a word on the afresh row lands");
    assert_eq!(row.request, Request::Skip("2.1.282".to_string()));
    let now = now_s();
    let need = Row::of(ID, &stopped(fresh, ""), now)
        .interrupt(now)
        .expect("its agent is down");
    assert_eq!(need.command.as_deref(), Some("claude"));
    let homed = " CLAUDE_CONFIG_DIR='/alt/.claude' '/opt/claude' '--dangerously-skip-permissions'";
    let row = Row::of(ID, &stopped(fresh, homed), now);
    assert_eq!(
        row.fresh_command().as_deref(),
        Some("CLAUDE_CONFIG_DIR=/alt/.claude claude")
    );
    assert_eq!(
        row.interrupt(now).and_then(|n| n.command).as_deref(),
        Some("CLAUDE_CONFIG_DIR=/alt/.claude claude")
    );
    save(&o, ID, &stopped("", ""));
    let e = ask(&o, TAB, Ask::Now, false).expect_err("not re-armed");
    assert!(e.contains(&format!("`claude --resume {ID}`")), "{e}");
}

#[test]
fn an_ask_writes_the_owners_word_under_the_lock_and_refuses_what_it_cannot_hold() {
    let dir = ScratchDir::new("upgrade-status", "ask");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 5,
        ..St::default()
    };
    save(&o, "aaa", &st);
    let row = ask(&o, TAB, Ask::Now, false).expect("the ask lands");
    assert_eq!(row.request, Request::Now);
    let written = load(&o, "aaa").expect("state");
    assert_eq!(written.request, Request::Now);
    assert_eq!(written.request_tab, TAB, "the word is on the tab named");
    assert_eq!(
        written.request_for("s-elsewhere"),
        Request::None,
        "the same conversation in another tab carries none of it"
    );
    let ledger = std::fs::read_to_string(super::super::ledger_path(&o)).expect("ledger");
    assert!(ledger.contains(r#""step":"requested:now""#), "{ledger}");

    let row = ask(&o, TAB, Ask::Skip, false).expect("skip");
    assert_eq!(row.request, Request::Skip("2.1.282".to_string()));
    let row = ask(&o, TAB, Ask::Defer(3_600), false).expect("defer");
    assert!(matches!(row.request, Request::DeferUntil(t) if t >= now_s() + 3_590));

    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("unanswered".to_string()),
            marker: "READY-x".to_string(),
            ..st.clone()
        },
    );
    let row = ask(&o, TAB, Ask::Now, false).expect("re-armed");
    assert_eq!(row.phase, Phase::Pending);
    assert!(load(&o, "aaa").expect("state").marker.is_empty());

    for why in [
        "not-a-shell-job",
        "argv:--print",
        "no-resume",
        "resumed-elsewhere",
    ] {
        let stopped = St {
            phase: Phase::Failed(why.to_string()),
            ..st.clone()
        };
        save(&o, "aaa", &stopped);
        let stopped = load(&o, "aaa").expect("saved");
        let _ = ask(&o, TAB, Ask::Now, false).expect_err("not re-armed");
        assert_eq!(load(&o, "aaa"), Some(stopped), "{why}: nothing written");
        let row = ask(&o, TAB, Ask::Skip, false).expect("a skip ends it");
        assert_eq!(row.stall(now_s()), None, "{why}");
    }
    save(&o, "aaa", &st);

    let _ = ask(&o, "s-0000", Ask::Now, false).expect_err("no upgrade there");

    save(
        &o,
        "ccc",
        &St {
            phase: Phase::Exiting { at_s: NOW },
            tab: "s-busy".to_string(),
            ..st.clone()
        },
    );
    let _ = ask(&o, "s-busy", Ask::Defer(60), false).expect_err("in flight");
    assert_eq!(
        load(&o, "ccc").expect("state").request,
        Request::None,
        "nothing written"
    );

    let held = super::super::sweep_lock(&o).expect("lock").expect("held");
    let before = load(&o, "aaa");
    let wait = std::time::Duration::from_millis(300);
    let e =
        ask_within(&o, TAB, Ask::Skip, None, wait, &|s| holders(&o, s, None)).expect_err("busy");
    assert_eq!(e, "busy:another-sweep");
    assert_eq!(load(&o, "aaa"), before, "nothing written while busy");
    drop(held);
}

#[test]
fn a_word_for_a_target_that_moved_on_lands_on_nothing() {
    let dir = ScratchDir::new("upgrade-status", "ask-for");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 5,
        ..St::default()
    };
    save(&o, "aaa", &st);
    let file = state_dir(&o).join("aaa.json");
    let before = std::fs::read_to_string(&file).expect("state file");
    for word in [Ask::Now, Ask::Defer(24 * 3_600), Ask::Skip] {
        let e = ask_for(&o, TAB, word, "2.1.282", "").expect_err("the row moved on");
        assert_eq!(e, "stale:2.1.283", "{word:?}");
        assert_eq!(
            std::fs::read_to_string(&file).expect("state file"),
            before,
            "{word:?}: nothing written"
        );
        assert_eq!(load(&o, "aaa").expect("state").request, Request::None);
    }
    assert!(
        !super::super::ledger_path(&o).exists(),
        "no word is ledgered"
    );
    assert!(!word_marker(&o.state).exists(), "no word is announced");

    let row = ask_for(&o, TAB, Ask::Skip, "2.1.283", "").expect("the build shown");
    assert_eq!(row.request, Request::Skip("2.1.283".to_string()));
    let row = ask_for(&o, TAB, Ask::Now, "2.1.283.0", "").expect("the same build");
    assert_eq!(row.request, Request::Now);
    assert_eq!(
        load(&o, "aaa").expect("state").request,
        Request::Now,
        "written"
    );
}

#[test]
fn a_queued_word_requires_the_displayed_source_scope_before_writing() {
    let shown = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        proc_start: "A".to_string(),
        salt: 5,
        ..St::default()
    };
    let scope = AskScope::from(&Row::of("aaa", &shown, NOW));
    for current in [
        St {
            from: "2.1.282".to_string(),
            ..shown.clone()
        },
        St {
            source: "native".to_string(),
            ..shown.clone()
        },
        St {
            agent: upgrade::Agent::Muse,
            ..shown.clone()
        },
        St {
            proc_start: "B".to_string(),
            ..shown.clone()
        },
        St {
            proc_start: String::new(),
            ..shown.clone()
        },
    ] {
        for what in [Ask::Now, Ask::Press, Ask::Defer(24 * 3_600), Ask::Skip] {
            let dir = ScratchDir::new("upgrade-status", "queued-scope");
            let o = opts(&dir);
            save(&o, "aaa", &current);
            let file = state_dir(&o).join("aaa.json");
            let before = std::fs::read(&file).expect("fresh record");
            let result = ask_for_scope(&o, TAB, what, "2.1.283", "aaa", &scope);
            assert!(result.is_err(), "{what:?}: {current:?}: {result:?}");
            assert_eq!(
                std::fs::read(&file).expect("fresh record after refusal"),
                before,
                "{what:?}: nothing written to the changed scope"
            );
            assert!(!super::super::ledger_path(&o).exists(), "no word ledgered");
            assert!(!word_marker(&o.state).exists(), "no word announced");
        }
    }
}

#[test]
fn a_queued_word_accepts_the_same_scope_and_compatible_identity_evidence() {
    for (shown_from, current_from, shown_birth, current_birth, session) in [
        ("2.1.281", "2.1.281", "A", "A", "aaa"),
        ("2.1.281.0", "v2.1.281", "A", "A", "aaa"),
        ("opaque-source", "opaque-source", "A", "A", "aaa"),
        ("-", "-", "", "", "aaa"),
        ("2.1.281", "2.1.281", "", "B", "aaa"),
        ("2.1.281", "2.1.281", "A", "A", "missing"),
    ] {
        for what in [Ask::Now, Ask::Press, Ask::Defer(24 * 3_600), Ask::Skip] {
            let dir = ScratchDir::new("upgrade-status", "queued-same-scope");
            let o = opts(&dir);
            let current = St {
                phase: Phase::Pending,
                from: current_from.to_string(),
                to: "2.1.283".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                proc_start: current_birth.to_string(),
                salt: 5,
                ..St::default()
            };
            let scope = AskScope {
                from: shown_from.to_string(),
                proc_start: shown_birth.to_string(),
                ..AskScope::from(&Row::of("aaa", &current, NOW))
            };
            save(&o, "aaa", &current);
            let row = ask_for_scope(&o, TAB, what, "2.1.283.0", session, &scope)
                .expect("the same displayed scope accepts the word");
            assert_eq!(row.session, "aaa", "missing conversation keeps tab ranking");
            match what {
                Ask::Now | Ask::Press => assert_eq!(row.request, Request::Now),
                Ask::Defer(_) => assert!(matches!(row.request, Request::DeferUntil(_))),
                Ask::Skip => assert_eq!(row.request, Request::Skip("2.1.283".to_string())),
            }
            let written = load(&o, "aaa").expect("accepted word saved");
            assert_eq!(written.request, row.request);
            assert_eq!(written.request_tab, TAB);
            assert!(
                super::super::ledger_path(&o).exists(),
                "accepted word ledgered"
            );
            assert!(word_marker(&o.state).exists(), "accepted word announced");
        }
    }
}

#[test]
fn an_unknown_probe_keeps_the_birth_a_queued_word_captured() {
    for what in [Ask::Now, Ask::Press, Ask::Defer(24 * 3_600), Ask::Skip] {
        let dir = ScratchDir::new("upgrade-status", "queued-known-birth");
        let o = opts(&dir);
        let st = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            proc_start: "A".to_string(),
            ..St::default()
        };
        let scope = AskScope::from(&Row::of("aaa", &st, NOW));
        save(&o, "aaa", &st);
        super::super::retarget_pending_for_test(&o, "aaa", "2.1.281", "")
            .expect("the real pending retarget with an unreadable later birth");
        let current = load(&o, "aaa").expect("retargeted record");
        assert_eq!(current.proc_start, "A", "known birth is retained");
        assert_eq!(current.request, Request::None);
        let row = ask_for_scope(&o, TAB, what, "2.1.283", "aaa", &scope)
            .expect("the retained birth still names the displayed run");
        assert_ne!(row.request, Request::None, "the word was written");
    }
}

#[test]
fn an_owners_word_that_lands_wakes_the_window() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = ScratchDir::new("upgrade-status", "word-push");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let marker = word_marker(&o.state);
    assert!(!marker.exists(), "no word yet");
    let _ = ask(&o, TAB, Ask::Now, false).expect_err("nothing recorded");
    assert!(!marker.exists(), "a refused word wakes nobody");
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            salt: 5,
            ..St::default()
        },
    );
    ask(&o, TAB, Ask::Now, false).expect("lands");
    let first = std::fs::metadata(&marker)
        .expect("the word announced")
        .ino();
    ask(&o, TAB, Ask::Skip, false).expect("lands again");
    let second = std::fs::metadata(&marker).expect("announced again").ino();
    assert_ne!(first, second, "a new file each time: its identity changes");
}

#[test]
fn a_relaunchs_record_is_no_upgrade_of_the_tab() {
    let dir = ScratchDir::new("upgrade-status", "relaunch-records");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let upgrade = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 5,
        ..St::default()
    };
    for (session, cause) in [
        ("exit", super::super::super::relaunch::CAUSE_EXIT),
        ("memory", super::super::super::relaunch::CAUSE_MEMORY),
    ] {
        save(
            &o,
            session,
            &St {
                cause: cause.to_string(),
                salt: 9,
                ..upgrade.clone()
            },
        );
    }
    assert!(rows_at(&o, NOW).is_empty(), "the relaunch's records only");
    let _ = ask(&o, TAB, Ask::Now, false).expect_err("no upgrade there");
    save(&o, "zzz", &upgrade);
    let rows = rows_at(&o, NOW);
    assert_eq!(
        rows.iter().map(|r| r.session.as_str()).collect::<Vec<_>>(),
        ["zzz"]
    );
    let row = ask(&o, TAB, Ask::Now, false).expect("the upgrade's record");
    assert_eq!(row.session, "zzz");
    assert_eq!(load(&o, "exit").expect("kept").request, Request::None);
}

#[test]
fn the_view_names_the_instant_it_next_changes() {
    assert_eq!(
        next_change(&[pending(3_600)], NOW),
        Some(NOW - 3_600 + STALLED_AFTER_S),
        "turning overdue"
    );
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 60,
        ..pending(7 * 3_600)
    };
    assert_eq!(
        next_change(&[hurried], NOW),
        Some(NOW - 60 + NOW_QUIETS_S),
        "the word stops quieting it"
    );
    let deferred = Row {
        request: Request::DeferUntil(NOW + 90),
        ..pending(60)
    };
    assert_eq!(
        next_change(std::slice::from_ref(&deferred), NOW),
        Some(NOW + 90)
    );
    let done = Row {
        phase: Phase::Done,
        done_at: NOW - 30,
        ..pending(60)
    };
    assert_eq!(
        next_change(&[done, deferred], NOW),
        Some(NOW + 90).min(Some(NOW - 30 + DONE_SHOWN_S + 1)),
        "the earliest"
    );
    let restarting = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(60)
    };
    assert_eq!(
        next_change(&[restarting], NOW),
        Some(NOW + super::super::STALE_S + 1)
    );
    assert_eq!(next_change(&[], NOW), None);
}

#[test]
fn the_view_names_a_records_watch_and_lists_it_due_once_past() {
    let dir = ScratchDir::new("upgrade-status", "watch-due");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 60,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW);
    super::super::save_at(
        &o,
        "zzz",
        &St {
            tab: "s-elsewhere".to_string(),
            ..st.clone()
        },
        NOW,
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut view = View::default();
    let due = NOW + upgrade::REASK_S;
    assert_eq!(
        view.refresh_with(&o, &tabs, NOW, &held, &mut |_| {}),
        Some(due),
        "the pending round's watch is the instant named"
    );
    assert!(view.watch_due().is_empty(), "not yet due");
    assert_eq!(
        view.refresh_with(&o, &tabs, NOW, &|_| None, &mut |_| {}),
        Some(due),
        "holders that do not read still leave the watch's instant named"
    );
    let _ = view.refresh_with(&o, &tabs, due, &held, &mut |_| {});
    assert_eq!(view.watch_due(), [TAB], "due, and only the live tab");
    let _ = view.refresh_with(&o, &tabs, due, &|_| None, &mut |_| {});
    assert_eq!(view.watch_due(), [TAB], "the records alone decide");
    let _ = view.refresh_with(&o, &[], due, &held, &mut |_| {});
    assert!(view.watch_due().is_empty(), "no live tab, none due");
    let locked = state_dir(&o).join("unreadable.json");
    std::fs::write(&locked, "{}").expect("a record");
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000))
        .expect("chmod");
    assert_eq!(
        view.refresh_with(&o, &tabs, NOW, &held, &mut |_| {}),
        Some(due),
        "records not read whole: the readable record's instant is named"
    );
    let _ = view.refresh_with(&o, &tabs, due, &held, &mut |_| {});
    assert_eq!(view.watch_due(), [TAB], "records not read whole: still due");
    let _ = std::fs::remove_file(&locked);
    view.stand_down(&mut |_| {});
    assert!(view.watch_due().is_empty());
}

#[test]
fn a_record_nothing_has_checked_reads_unwatched_past_the_wake_grace() {
    use crate::harness::upgrade_drive::WATCH_GAP;
    let long_awake = NOW - 86_400;
    let looked = NOW - UNWATCHED_AFTER_S - 60;
    let due = looked + WATCH_GAP;
    assert_eq!(unwatched_since(due, looked, NOW, long_awake), Some(looked));
    let first = NOW - WATCH_GAP - 60;
    assert_eq!(unwatched_since(first, 0, NOW, long_awake), Some(first));
    assert_eq!(unwatched_since(0, looked, NOW, long_awake), None);
    assert_eq!(unwatched_since(due, looked, NOW, 0), None);
    let rested = NOW - 10 * 3_600;
    assert!(NOW - rested > UNWATCHED_AFTER_S, "the bound alone flags it");
    assert_eq!(
        unwatched_since(NOW - 1, rested, NOW, long_awake),
        None,
        "its watch is a second late"
    );
    assert_eq!(
        unwatched_since(NOW - WATCH_GAP - 1, rested, NOW, long_awake),
        Some(rested),
        "a whole gap late"
    );
    for (watch_at, looked_at, awake) in [
        (due, looked, long_awake),
        (NOW - 1, rested, long_awake),
        (NOW + 600, NOW + 600 - WATCH_GAP, long_awake),
        (due, looked, NOW - 60),
        (first, 0, long_awake),
    ] {
        let t = unwatched_turns_at(watch_at, looked_at, awake).expect("owed, awake known");
        assert_eq!(
            unwatched_since(watch_at, looked_at, t - 1, awake),
            None,
            "{watch_at} {looked_at} {awake}: not before {t}"
        );
        assert!(
            unwatched_since(watch_at, looked_at, t, awake).is_some(),
            "{watch_at} {looked_at} {awake}: at {t}"
        );
    }
    assert_eq!(unwatched_turns_at(0, looked, long_awake), None);
    assert_eq!(unwatched_turns_at(due, looked, 0), None);
}

#[test]
fn the_awake_clock_is_the_machines_last_wake_or_boot() {
    let at = system_awake_since();
    let now = now_s();
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(
            at > 1_000_000_000 && at <= now,
            "awake since {at}, now {now}"
        );
    } else {
        assert_eq!(at, 0, "no awake clock here");
    }
}

#[test]
fn an_unwatched_record_is_told_as_aterms_own_fault() {
    use crate::harness::upgrade_drive::WATCH_GAP;
    let unread = |row: Row| Row {
        watch_at: NOW - WATCH_GAP - 600,
        looked_at: NOW - UNWATCHED_AFTER_S - 600,
        awake_since: NOW - 86_400,
        ..row
    };
    let healthy = pending(3_600);
    assert_eq!(healthy.stall(NOW), None, "an hour behind is healthy");
    let row = unread(healthy.clone());
    assert_eq!(row.unwatched(NOW), Some(NOW - UNWATCHED_AFTER_S - 600));
    assert_eq!(row.stall(NOW).as_deref(), Some("unwatched"));
    let words = row.stall_words(NOW).expect("stalled");
    assert!(words.chars().count() <= 64, "{words}");
    assert_eq!(row.badge(NOW), None, "aterm's own fault marks no tab");
    let Attention::Fault(need) = row.attention(NOW) else {
        panic!("a fault line: {:?}", row.attention(NOW));
    };
    assert_eq!(need.kind, "unwatched");
    assert_eq!(
        row.attention(NOW + 3 * 3_600),
        row.attention(NOW),
        "the line says the bound, never the running age"
    );
    assert_eq!(row.remedy(NOW), Some(Remedy::Waits), "no word mends it");
    assert!(!row.moving(NOW));
    assert_eq!(row.column(NOW), "stalled/2.1.282/unwatched:1h10m/1h");
    assert_eq!(row.age_said(NOW).as_deref(), Some("1 h"));
    let rows = [Row {
        behind_said: row.age_said(NOW),
        ..row.clone()
    }];
    let next = next_change(&rows, NOW).expect("the age's next word");
    assert_eq!(next, NOW - UNWATCHED_AFTER_S - 600 + 2 * 3_600);
    assert_eq!(row.age_said(next).as_deref(), Some("2 h"));
    let records = [
        Row {
            phase: Phase::Announced {
                at_s: NOW - 600,
                asks: 1,
            },
            wait: "background".to_string(),
            ..pending(7 * 3_600)
        },
        Row {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            retry_at: NOW + 3_600,
            ..pending(7 * 3_600)
        },
        Row {
            wait: "limited".to_string(),
            limit_until: NOW + 3_600,
            ..pending(7 * 3_600)
        },
    ];
    for record in records {
        assert!(record.asks_on_its_own(NOW), "{record:?}");
        assert!(record.badge(NOW).is_none(), "a record: {record:?}");
        let told = unread(record);
        assert!(!told.asks_on_its_own(NOW), "{told:?}");
        assert_eq!(told.stall(NOW).as_deref(), Some("unwatched"), "{told:?}");
        assert!(
            matches!(told.attention(NOW), Attention::Fault(ref n) if n.kind == "unwatched"),
            "{told:?}"
        );
        assert!(told.badge(NOW).is_none(), "{told:?}");
        assert_eq!(told.remedy(NOW), Some(Remedy::Waits), "{told:?}");
    }
    for (why, other) in [
        (
            "finished",
            Row {
                phase: Phase::Done,
                done_at: NOW - 60,
                ..row.clone()
            },
        ),
        (
            "under way",
            Row {
                phase: Phase::Exiting { at_s: NOW - 60 },
                ..row.clone()
            },
        ),
        (
            "stopped after its agent was gone",
            Row {
                phase: Phase::Failed("no-resume-hint".to_string()),
                exited_at: NOW - 600,
                ..row.clone()
            },
        ),
    ] {
        assert_eq!(other.unwatched(NOW), None, "{why}");
        assert_ne!(other.stall(NOW).as_deref(), Some("unwatched"), "{why}");
    }
    let skipped = Row {
        request: Request::Skip("2.1.282".to_string()),
        ..row.clone()
    };
    assert_eq!(skipped.stall(NOW), None);
    assert!(skipped.badge(NOW).is_none());
}

#[test]
fn the_view_marks_a_record_nothing_checks_at_an_instant_it_names() {
    use crate::harness::upgrade_drive::WATCH_GAP;
    let dir = ScratchDir::new("upgrade-status", "unwatched");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 60,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let due = NOW + upgrade::REASK_S;
    let turns = due + WATCH_GAP + 1;
    let look = |awake: u64, at: u64| {
        pin_awake_since(awake);
        let mut view = View::default();
        let mut sent: Vec<Row> = Vec::new();
        let next = view.refresh_with(&o, &tabs, at, &held, &mut |rows| sent = rows.to_vec());
        (next, sent, view.watch_due().to_vec())
    };
    let long_awake = NOW - 86_400;
    let (next, _, _) = look(long_awake, NOW);
    assert_eq!(next, Some(due), "the watch first");
    let (next, sent, watch) = look(long_awake, due);
    assert_eq!(watch, [TAB], "due at its instant");
    assert_eq!(sent[0].stall(due), None, "a record while its watch is due");
    assert_eq!(next, Some(turns), "then the instant it turns, named");
    let (_, sent, watch) = look(long_awake, turns);
    assert_eq!(watch, [TAB], "still listed due: the host keeps trying");
    assert_eq!(sent[0].stall(turns).as_deref(), Some("unwatched"));
    assert_eq!(sent[0].stalled.as_deref(), Some("unwatched"), "sent so");
    assert!(
        matches!(sent[0].attention(turns), Attention::Fault(ref n) if n.kind == "unwatched"),
        "aterm's own fault: a line in the log"
    );
    assert!(sent[0].badge(turns).is_none(), "and no tab is marked");
    let (next, _, _) = look(0, due);
    assert_ne!(next, Some(turns));
    let (_, sent, _) = look(0, turns);
    assert_eq!(sent[0].stall(turns), None);
    let woke = turns - 60;
    let (next, sent, _) = look(woke, turns);
    assert_eq!(sent[0].stall(turns), None, "inside the wake grace");
    assert_eq!(next, Some(woke + WAKE_GRACE_S), "the grace's end, named");
    let mut looked = super::super::load(&o, "aaa").expect("the record");
    looked.looked_at = due;
    super::super::save_at(&o, "aaa", &looked, due);
    let (next, sent, watch) = look(long_awake, due + 1);
    assert!(watch.is_empty(), "watched: not due again until a gap on");
    assert_eq!(sent[0].stall(due + 1), None);
    assert_eq!(next, Some(due + WATCH_GAP), "its next watch");
    let (next, sent, watch) = look(long_awake, turns);
    assert_eq!(watch, [TAB], "due again");
    assert_eq!(sent[0].stall(turns), None, "a second late, not unwatched");
    assert_eq!(
        next,
        Some(due + 2 * WATCH_GAP + 1),
        "unless that one is missed"
    );
    pin_awake_since(0);
}

#[test]
fn a_host_that_has_just_begun_or_resumed_watching_flags_nothing_unwatched() {
    use crate::harness::upgrade_drive::WATCH_GAP;
    use std::time::{Duration, Instant};
    let dir = ScratchDir::new("upgrade-status", "unwatched-host");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 60,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let turns = NOW + upgrade::REASK_S + WATCH_GAP + 1;
    pin_awake_since(NOW - 86_400);
    let t0 = Instant::now();
    let after = |s: u64| t0 + Duration::from_secs(s);
    let look = |view: &mut View, at: u64| {
        let mut sent: Vec<Row> = Vec::new();
        let next = view.refresh_with(&o, &tabs, at, &held, &mut |rows| sent = rows.to_vec());
        (next, sent.remove(0))
    };
    let (_, row) = look(&mut View::default(), turns);
    assert_eq!(row.stalled.as_deref(), Some("unwatched"), "main flashes");

    let mut view = View::default();
    view.note_look(t0, turns);
    let (next, row) = look(&mut view, turns);
    assert_eq!(row.stalled, None, "{row:?}");
    assert_eq!(row.stall(turns), None);
    assert_eq!(next, Some(turns + WAKE_GRACE_S), "the grace's end, named");
    let later = turns + WAKE_GRACE_S;
    view.note_look(after(WAKE_GRACE_S), later);
    let (_, row) = look(&mut view, later);
    assert_eq!(row.stalled.as_deref(), Some("unwatched"), "told past it");
    let resumed = later + 8 * 3_600;
    view.note_look(after(WAKE_GRACE_S + 60), resumed);
    let (_, row) = look(&mut view, resumed);
    assert_eq!(row.unwatched(resumed), None, "the first look after a sleep");
    assert_eq!(row.stalled.as_deref(), Some("overdue"), "its own stall");
    let mut awake = View::default();
    awake.note_look(t0, turns - 8 * 3_600);
    awake.note_look(after(8 * 3_600), turns);
    let (_, row) = look(&mut awake, turns);
    assert_eq!(
        row.stalled.as_deref(),
        Some("unwatched"),
        "no sleep between"
    );
    view.stand_down(&mut |_| {});
    let back = resumed + 3_600;
    view.note_look(after(WAKE_GRACE_S + 60 + 3_600), back);
    let (_, row) = look(&mut view, back);
    assert_eq!(row.unwatched(back), None, "the first look back on");
    assert_eq!(row.stalled.as_deref(), Some("overdue"), "its own stall");
    pin_awake_since(0);
}

#[cfg(unix)]
fn jobs_on_a_tty(
    dir: &std::path::Path,
    tab: &str,
) -> (
    std::process::Child,
    u32,
    (std::os::fd::OwnedFd, std::os::fd::OwnedFd),
) {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned here alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let exe = std::env::current_exe().expect("exe");
    let park = format!(
        "'{}' harness::upgrade_drive::tests::park_when_asked --exact </dev/null >/dev/null 2>&1",
        exe.display()
    );
    let pid_file = dir.join("back.pid");
    let mut shell = std::process::Command::new("/bin/sh");
    shell
        .inherit_nothing()
        .args([
            "-c",
            &format!(
                "set -m; {park} & printf '%s\\n' \"$!\" > '{}'; exec {park}",
                pid_file.display()
            ),
        ])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    // SAFETY: pre-exec closure calls only async-signal-safe `setsid` and `ioctl(TIOCSCTTY)`.
    unsafe {
        shell.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut front = shell.spawn().expect("sh");
    let parked = |pid: u32| {
        atpkg::caller_shell::process_args(pid).is_some_and(|a| {
            a.env_var("UPGRADE_DRIVE_TEST_PARK").is_some()
                && std::path::Path::new(&a.exec_path) == exe.as_path()
        })
    };
    for _ in 0..3_000 {
        let back = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        if let Some(back) = back.filter(|&b| parked(b) && parked(front.id())) {
            return (front, back, (master, slave));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = front.kill();
    drop((master, slave));
    let _ = front.wait();
    panic!("the two jobs never ran");
}

#[cfg(unix)]
#[test]
fn a_tab_whose_agent_is_not_in_front_is_never_told_aterms_own_fault() {
    const BACK: &str = "0badf00d-aaaa-2222-3333-444455556666";
    const FRONT: &str = "0badf00d-bbbb-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "aside");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let (mut front, back, pty) = jobs_on_a_tty(&dir, TAB);
    register_as(&o.home, back, BACK, "2.1.281");
    register_as(&o.home, front.id(), FRONT, "2.1.281");
    let group = super::super::process_group(front.id()).expect("group");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(group),
    }];
    let now = now_s();
    for session in [BACK, FRONT] {
        let st = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            pending_since: now - 7 * 3_600,
            looked_at: now - 3 * 3_600,
            ..St::default()
        };
        super::super::save_at(&o, session, &st, now - 7 * 3_600);
    }
    pin_awake_since(now - 86_400);
    let sessions: BTreeSet<String> = [BACK, FRONT].map(str::to_string).into();
    let in_window = holders(&o, &sessions, Some(&tabs));
    let by_cli = holders(&o, &sessions, None);
    let (cli, vetted) = status_rows(&o, true);
    let mut view = View::default();
    let mut sent: Vec<Row> = Vec::new();
    let _ = view.refresh_with(
        &o,
        &tabs,
        now,
        &|s| holders(&o, s, Some(&tabs)),
        &mut |rows| sent = rows.to_vec(),
    );
    // SAFETY: `kill` sends SIGKILL to the pid this test's jobs reported.
    unsafe {
        libc::kill(libc::pid_t::try_from(back).expect("pid"), libc::SIGKILL);
    }
    let _ = front.kill();
    drop(pty);
    let _ = front.wait();
    pin_awake_since(0);

    for (how, held) in [("window", in_window), ("cli", by_cli)] {
        let held = held.expect("the session files read");
        let of = |s: &str| held.iter().find(|h| h.session == s).cloned().expect(s);
        assert_eq!(
            of(BACK).tab.as_deref(),
            Some(TAB),
            "{how}: proven in its tab"
        );
        assert!(of(BACK).aside, "{how}: not in front");
        assert_eq!(of(FRONT).tab.as_deref(), Some(TAB), "{how}");
        assert!(!of(FRONT).aside, "{how}: NEGATIVE CONTROL, in front");
    }
    assert!(vetted, "the CLI's holders read");
    for (how, rows) in [("cli", &cli), ("window", &sent)] {
        let of = |s: &str| rows.iter().find(|r| r.session == s).expect(s);
        let (aside, front) = (of(BACK), of(FRONT));
        assert_eq!(
            front.stall(now).as_deref(),
            Some("unwatched"),
            "{how}: NEGATIVE CONTROL, its tab's agent"
        );
        assert_eq!(aside.aside, NOT_FOREGROUND, "{how}");
        assert_eq!(aside.wait, NOT_FOREGROUND, "{how}: what holds it");
        assert_eq!(aside.unwatched(now), None, "{how}");
        assert_eq!(aside.stall(now).as_deref(), Some("overdue"), "{how}");
        assert!(aside.badge(now).is_none(), "{how}: no tab mark");
        let stood = Row {
            wait_since: now - SUSPENDED_AFTER_S - 60,
            ..aside.clone()
        };
        let Attention::Digest(need) = stood.attention(now) else {
            panic!("{how}: one digest line: {:?}", stood.attention(now));
        };
        assert_eq!(need.kind, "suspended", "{how}");
        assert_eq!(aside.remedy(now), Some(Remedy::Waits), "{how}");
        assert!(
            !aside.asks_on_its_own(now),
            "{how}: a row, the person's to mend"
        );
        assert_eq!(aside.cause_word(now), "person", "{how}");
    }
}

fn claims(claimed: &BTreeSet<String>) -> Aside {
    Aside {
        claimed: claimed.clone(),
        ..Aside::default()
    }
}

#[test]
fn a_tab_another_supervisor_claims_reads_its_claim_never_aterms_own_fault() {
    let dir = ScratchDir::new("upgrade-status", "claimed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 7 * 3_600,
        looked_at: NOW - 3 * 3_600,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW - 7 * 3_600);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    pin_awake_since(NOW - 86_400);
    let look = |view: &mut View, claimed: &[&str]| {
        let claimed: Vec<String> = claimed.iter().map(|t| (*t).to_string()).collect();
        view.set_claimed(&claimed);
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
        sent.remove(0)
    };
    let unclaimed = look(&mut View::default(), &[]);
    assert_eq!(
        unclaimed.stall(NOW).as_deref(),
        Some("unwatched"),
        "NEGATIVE CONTROL"
    );
    let elsewhere = look(&mut View::default(), &["s-another"]);
    assert_eq!(
        elsewhere.stall(NOW).as_deref(),
        Some("unwatched"),
        "another tab's claim"
    );
    let mut view = View::default();
    let row = look(&mut view, &[TAB]);
    assert_eq!((row.aside.as_str(), row.wait.as_str()), (CLAIMED, CLAIMED));
    assert_eq!(row.unwatched(NOW), None);
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    assert!(
        row.badge(NOW).is_none(),
        "another supervisor's claim marks no tab"
    );
    assert!(
        matches!(row.attention(NOW), Attention::Quiet),
        "the owner's own setup, a record: {:?}",
        row.attention(NOW)
    );
    view.stand_down(&mut |_| {});
    let mut sent: Vec<Row> = Vec::new();
    let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
    assert_eq!(
        sent[0].stall(NOW).as_deref(),
        Some("unwatched"),
        "forgotten"
    );
    pin_awake_since(0);
}

#[test]
fn a_tab_whose_autopilot_is_off_reads_its_off_never_aterms_own_fault() {
    let dir = ScratchDir::new("upgrade-status", "autopilot-off");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 7 * 3_600,
        looked_at: NOW - 3 * 3_600,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW - 7 * 3_600);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    pin_awake_since(NOW - 86_400);
    let strings = |tabs: &[&str]| tabs.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
    let look = |view: &mut View, claimed: &[&str], off: &[&str]| {
        view.set_claimed(&strings(claimed));
        view.set_off(&strings(off));
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
        sent.remove(0)
    };
    for off in [&[][..], &["s-another"][..]] {
        assert_eq!(
            look(&mut View::default(), &[], off).stall(NOW).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {off:?}"
        );
    }
    let mut view = View::default();
    let row = look(&mut view, &[], &[TAB]);
    assert_eq!(
        (row.aside.as_str(), row.wait.as_str()),
        (AUTOPILOT_OFF, AUTOPILOT_OFF)
    );
    assert_eq!(row.unwatched(NOW), None);
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    assert!(row.badge(NOW).is_none(), "the owner's own Off marks no tab");
    assert!(
        matches!(row.attention(NOW), Attention::Quiet),
        "the owner's own setup, a record: {:?}",
        row.attention(NOW)
    );
    let cause = row.overdue_cause(NOW);
    assert_eq!(cause, "its autopilot is off");
    let words = row.stall_words(NOW).expect("stalled");
    assert!(words.ends_with("its autopilot is off"), "{words}");
    assert!(
        row.waiting_line("09:00")
            .contains("it moves once its autopilot is cruise or land again"),
        "{}",
        row.waiting_line("09:00")
    );
    let both = look(&mut View::default(), &[TAB], &[TAB]);
    assert_eq!(
        (both.aside.as_str(), both.wait.as_str()),
        (CLAIMED, CLAIMED)
    );
    view.stand_down(&mut |_| {});
    let mut sent: Vec<Row> = Vec::new();
    let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
    assert_eq!(
        sent[0].stall(NOW).as_deref(),
        Some("unwatched"),
        "forgotten"
    );
    let cli = |off: &[&str]| {
        let aside = Aside {
            off: off.iter().map(|t| (*t).to_string()).collect(),
            ..Aside::default()
        };
        let (mut rows, vetted) = status_rows_with(&o, NOW, &held, &|| aside.clone());
        assert!(vetted);
        rows.remove(0)
    };
    let by_cli = cli(&[TAB]);
    assert_eq!(
        (by_cli.aside.as_str(), by_cli.stall(NOW)),
        (row.aside.as_str(), row.stall(NOW)),
        "the window's facts"
    );
    assert!(
        by_cli.line(NOW).contains(" wait=autopilot-off "),
        "{}",
        by_cli.line(NOW)
    );
    for off in [&[][..], &["s-another"][..]] {
        assert_eq!(
            cli(off).stall(NOW).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {off:?}"
        );
    }
    pin_awake_since(0);
}

#[cfg(unix)]
#[test]
fn an_agent_the_upgrade_ended_is_said_down_in_a_tab_set_aside_too() {
    let dir = ScratchDir::new("upgrade-status", "down-aside");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let strings = |tabs: &[&str]| tabs.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
    let read = |off: &[&str],
                agent_off: &[&str],
                held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        let mut view = View::default();
        view.set_off(&strings(off));
        view.set_agent_off(&strings(agent_off));
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), held, &mut |rows| sent = rows.to_vec());
        let aside = Aside {
            off: off.iter().map(|t| (*t).to_string()).collect(),
            agent_off: agent_off.iter().map(|t| (*t).to_string()).collect(),
            ..Aside::default()
        };
        let (cli, vetted) = status_rows_with(&o, now_s(), held, &|| aside.clone());
        assert!(vetted);
        (sent, cli)
    };
    let stale = super::super::STALE_S;
    let downs = [
        (
            "stopped after its exit",
            Phase::Failed("stale-exit".to_string()),
            now_s() - 600,
        ),
        (
            "gone, its line untyped past the bound",
            Phase::Exiting {
                at_s: now_s() - stale - 60,
            },
            now_s() - stale - 30,
        ),
    ];
    for (what, phase, exited_at) in downs {
        let session = moved_state(&o, upgrade::Agent::Claude, phase, exited_at);
        for (how, off, agent_off) in [
            ("its Autopilot Off", &[TAB][..], &[][..]),
            ("its agent switched off", &[][..], &[TAB][..]),
            ("neither", &[][..], &[][..]),
        ] {
            let (window, cli) = read(off, agent_off, &nobody);
            for (reader, rows) in [("the window", &window), ("--status", &cli)] {
                assert_eq!(rows.len(), 1, "{what}, {how}, {reader}: {rows:?}");
                match rows[0].attention(now_s()) {
                    Attention::Interrupt(need) => {
                        assert_eq!(need.kind, "agent-down", "{what}, {how}, {reader}");
                        assert!(
                            need.command
                                .as_deref()
                                .is_some_and(|c| c.contains(&format!("--resume {session}"))),
                            "{what}, {how}, {reader}: the line that resumes it: {:?}",
                            need.command
                        );
                    }
                    other => panic!("{what}, {how}, {reader}: the owner is needed: {other:?}"),
                }
            }
        }
        std::fs::remove_file(state_dir(&o).join(format!("{session}.json"))).expect("the record");
    }
    let owed = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 7 * 3_600,
        looked_at: now_s() - 3 * 3_600,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &owed, now_s() - 7 * 3_600);
    for (how, off, agent_off, word) in [
        ("its Autopilot Off", &[TAB][..], &[][..], AUTOPILOT_OFF),
        ("its agent switched off", &[][..], &[TAB][..], AGENT_OFF),
    ] {
        let (window, cli) = read(off, agent_off, &behind_in(TAB));
        for (reader, rows) in [("the window", &window), ("--status", &cli)] {
            assert_eq!(rows.len(), 1, "{how}, {reader}: {rows:?}");
            assert_eq!(rows[0].aside, word, "{how}, {reader}");
            assert!(
                matches!(rows[0].attention(now_s()), Attention::Quiet),
                "NEGATIVE CONTROL, {how}, {reader}: {:?}",
                rows[0].attention(now_s())
            );
        }
    }
}

/// THE PERSON'S NOW KEEPS THE OFF ASIDES: no worker steps a tab its Autopilot Off or its agent's
/// switch holds off, so a row the person asked to move now (the tab menu's Upgrade Now) is set
/// aside for each exactly as one with no word is, in the window's rows and in `--status`'s, and
/// its wait names the true reason. NEGATIVE CONTROL: with neither held off, neither row is aside.
/// A STANDING PRESS PASSES THE OFF ASIDES, ONCE: the owner's press of the ⬆️ moves the tab by
/// its own lane, so while it stands an Autopilot Off or an agent switched off sets the row aside
/// no longer, in the window's rows and in `--status`'s; a tab another supervisor holds stays
/// aside. NEGATIVE CONTROL: a press that lapsed, or one for another build, keeps the off asides.
#[cfg(unix)]
#[test]
fn a_standing_press_passes_the_off_asides_once() {
    let dir = ScratchDir::new("upgrade-status", "press-aside");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let strings = |tabs: &[&str]| tabs.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
    let read = |off: &[&str], agent_off: &[&str], claimed: &[&str]| {
        let held = behind_in(TAB);
        let mut view = View::default();
        view.set_off(&strings(off));
        view.set_agent_off(&strings(agent_off));
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |rows| sent = rows.to_vec());
        let aside = Aside {
            off: off.iter().map(|t| (*t).to_string()).collect(),
            agent_off: agent_off.iter().map(|t| (*t).to_string()).collect(),
            claimed: claimed.iter().map(|t| (*t).to_string()).collect(),
        };
        let (cli, vetted) = status_rows_with(&o, now_s(), &held, &|| aside.clone());
        assert!(vetted);
        (sent, cli)
    };
    for (press, pressed_at, pressed_to, passes) in [
        ("a standing press", now_s() - 60, "2.1.282", true),
        (
            "CONTROL: a lapsed press",
            now_s() - upgrade::PRESS_S - 60,
            "2.1.282",
            false,
        ),
        (
            "CONTROL: a press for another build",
            now_s() - 60,
            "2.1.281",
            false,
        ),
    ] {
        let pressed = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 7 * 3_600,
            looked_at: now_s() - 3 * 3_600,
            request: Request::Now,
            request_tab: TAB.to_string(),
            request_at: pressed_at,
            pressed_at,
            pressed_to: pressed_to.to_string(),
            ..St::default()
        };
        super::super::save_at(&o, "aaa", &pressed, now_s() - 7 * 3_600);
        for (how, off, agent_off, word) in [
            ("its Autopilot Off", &[TAB][..], &[][..], AUTOPILOT_OFF),
            ("its agent switched off", &[][..], &[TAB][..], AGENT_OFF),
        ] {
            let (window, cli) = read(off, agent_off, &[]);
            for (reader, rows) in [("the window", &window), ("--status", &cli)] {
                assert_eq!(rows.len(), 1, "{press}, {how}, {reader}: {rows:?}");
                assert_eq!(
                    rows[0].aside,
                    if passes { "" } else { word },
                    "{press}, {how}, {reader}"
                );
                assert_eq!(rows[0].press_stands(now_s()), passes, "{press}");
            }
        }
        let (_, cli) = read(&[TAB], &[], &[TAB]);
        assert_eq!(cli[0].aside, CLAIMED, "{press}: a claimed tab stays aside");
    }
}

/// A row a press's answer brings keeps the shown row's aside ([`Row::keep_aside`]) but an
/// Autopilot or an agent switched off while its press stands, which the press passes; a claimed
/// tab stays aside. NEGATIVE CONTROL: with no press standing, every aside is kept.
#[test]
fn a_standing_press_keeps_no_off_aside() {
    let now = now_s();
    let row = |pressed_at: u64| Row {
        tab: TAB.to_string(),
        session: "aaa".to_string(),
        to: "2.1.282".to_string(),
        phase: Phase::Pending,
        request: Request::Now,
        pressed_at,
        pressed_to: "2.1.282".to_string(),
        ..Row::default()
    };
    for (aside, kept_when_pressed) in [(AUTOPILOT_OFF, false), (AGENT_OFF, false), (CLAIMED, true)]
    {
        let shown = Row {
            aside: aside.to_string(),
            ..row(0)
        };
        let mut pressed = row(now - 60);
        pressed.keep_aside(&shown, now);
        assert_eq!(
            pressed.aside,
            if kept_when_pressed { aside } else { "" },
            "{aside}"
        );
        let mut unpressed = row(0);
        unpressed.keep_aside(&shown, now);
        assert_eq!(unpressed.aside, aside, "CONTROL: {aside}");
    }
}

/// A PRESS ON A CLAIMED TAB IS REFUSED: no lane of aterm's moves a tab another supervisor holds,
/// so the press writes nothing and says why. NEGATIVE CONTROL: on a tab nobody else holds, the
/// press writes the Now and stamps the press for the build shown.
#[cfg(unix)]
#[test]
fn a_press_on_a_claimed_tab_is_refused() {
    let dir = ScratchDir::new("upgrade-status", "press-claimed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 5,
        ..St::default()
    };
    save(&o, "aaa", &st);
    let file = state_dir(&o).join("aaa.json");
    let before = std::fs::read_to_string(&file).expect("state file");
    let claimed = Aside {
        claimed: BTreeSet::from([TAB.to_string()]),
        ..Aside::default()
    };
    let e = ask_for_aside(&o, TAB, Ask::Press, "2.1.283", "", &|| claimed.clone())
        .expect_err("claimed");
    assert_eq!(e, PRESS_CLAIMED);
    assert_eq!(
        std::fs::read_to_string(&file).expect("state file"),
        before,
        "nothing written"
    );
    let row = ask_for_aside(&o, TAB, Ask::Press, "2.1.283", "", &Aside::default)
        .expect("CONTROL: an unclaimed tab");
    assert_eq!(row.request, Request::Now);
    assert!(row.press_stands(now_s()), "{row:?}");
    let saved = load(&o, "aaa").expect("state");
    assert_eq!(saved.pressed_to, "2.1.283");
    assert!(saved.pressed_at != 0);
    let now = ask_for_aside(&o, TAB, Ask::Now, "2.1.283", "", &|| claimed.clone())
        .expect("the Now reads no claim");
    assert!(
        !now.press_stands(now_s()),
        "the owner's Now after a press is no press: {now:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_persons_now_keeps_the_off_asides() {
    let dir = ScratchDir::new("upgrade-status", "now-aside");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let strings = |tabs: &[&str]| tabs.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
    let read = |off: &[&str], agent_off: &[&str]| {
        let held = behind_in(TAB);
        let mut view = View::default();
        view.set_off(&strings(off));
        view.set_agent_off(&strings(agent_off));
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |rows| sent = rows.to_vec());
        let aside = Aside {
            off: off.iter().map(|t| (*t).to_string()).collect(),
            agent_off: agent_off.iter().map(|t| (*t).to_string()).collect(),
            ..Aside::default()
        };
        let (cli, vetted) = status_rows_with(&o, now_s(), &held, &|| aside.clone());
        assert!(vetted);
        (sent, cli)
    };
    for request in [Request::Now, Request::None] {
        let owed = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 7 * 3_600,
            looked_at: now_s() - 3 * 3_600,
            request: request.clone(),
            request_tab: TAB.to_string(),
            request_at: now_s() - 60,
            ..St::default()
        };
        super::super::save_at(&o, "aaa", &owed, now_s() - 7 * 3_600);
        for (how, off, agent_off, word) in [
            ("its Autopilot Off", &[TAB][..], &[][..], AUTOPILOT_OFF),
            ("its agent switched off", &[][..], &[TAB][..], AGENT_OFF),
            ("CONTROL: neither", &[][..], &[][..], ""),
        ] {
            let (window, cli) = read(off, agent_off);
            for (reader, rows) in [("the window", &window), ("--status", &cli)] {
                assert_eq!(rows.len(), 1, "{request:?}, {how}, {reader}: {rows:?}");
                assert_eq!(rows[0].aside, word, "{request:?}, {how}, {reader}");
                if !word.is_empty() {
                    assert_eq!(
                        rows[0].wait, word,
                        "{request:?}, {how}, {reader}: the reason"
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn a_restarts_agent_no_look_saw_gone_is_read_gone_by_the_process_table() {
    let dir = ScratchDir::new("upgrade-status", "unseen-exit");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let gone = {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let session = "0badf00d-1111-2222-3333-444455556666";
    let signalled = now_s() - HUNG_EXIT_AFTER_S - 60;
    let read = |pid: u32| {
        save(
            &o,
            session,
            &St {
                agent: upgrade::Agent::Claude,
                phase: Phase::Exiting { at_s: signalled },
                from: "2.1.281".to_string(),
                to: "2.1.283".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: signalled - 600,
                pid,
                exited_at: 0,
                ..St::default()
            },
        );
        let mut view = View::default();
        view.set_off(&[TAB.to_string()]);
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |rows| {
            sent = rows.to_vec();
        });
        let aside = Aside {
            off: [TAB.to_string()].into(),
            ..Aside::default()
        };
        let (cli, vetted) = status_rows_with(&o, now_s(), &nobody, &|| aside.clone());
        assert!(vetted);
        [("the window", sent), ("--status", cli)].map(|(reader, rows)| {
            assert_eq!(rows.len(), 1, "{reader}: {rows:?}");
            match rows[0].attention(now_s()) {
                Attention::Interrupt(need) => (reader, need.kind, need.command),
                other => panic!("{reader}: the owner is needed: {other:?}"),
            }
        })
    };
    for (reader, kind, command) in read(gone) {
        assert_eq!(kind, "agent-down", "{reader}: its agent is gone");
        assert!(
            command
                .as_deref()
                .is_some_and(|c| c.contains(&format!("--resume {session}"))),
            "{reader}: the line that resumes it: {command:?}"
        );
    }
    for pid in [std::process::id(), 0] {
        for (reader, kind, command) in read(pid) {
            assert_eq!(kind, "hung-exit", "{reader}, pid {pid}");
            assert_eq!(command, None, "{reader}, pid {pid}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_slow_shutdown_just_ended_in_a_watched_tab_is_its_carrys_to_relaunch() {
    let dir = ScratchDir::new("upgrade-status", "slow-shutdown-watched");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let gone = {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let session = "0badf00d-1111-2222-3333-444455556666";
    let record = |signalled: u64| St {
        agent: upgrade::Agent::Claude,
        phase: Phase::Exiting { at_s: signalled },
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: signalled - 600,
        pid: gone,
        shell: std::process::id(),
        exited_at: 0,
        wait: "exiting".to_string(),
        ..St::default()
    };
    let read = || {
        let mut view = View::default();
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |rows| {
            sent = rows.to_vec()
        });
        let (cli, _) = status_rows_with(&o, now_s(), &nobody, &Aside::default);
        [("the window", sent), ("--status", cli)].map(|(reader, rows)| {
            assert_eq!(rows.len(), 1, "{reader}: {rows:?}");
            match rows[0].attention(now_s()) {
                Attention::Interrupt(need) => (reader, need.kind, need.command),
                Attention::Quiet => (reader, "quiet", None),
                other => panic!("{reader}: {other:?}"),
            }
        })
    };
    save(&o, session, &record(now_s() - super::super::STALE_S - 60));
    let lock = super::super::sweep_lock(&o).expect("lock").expect("held");
    let r = crate::harness::relaunch::carry_restart(&o, upgrade::Agent::Claude, Some(gone), false);
    assert!(r.step.starts_with("busy:"), "{r:?}");
    assert_eq!(
        load(&o, session).expect("kept").exited_at,
        0,
        "nothing stamped"
    );
    for (reader, kind, command) in read() {
        assert_eq!(
            (kind, command),
            ("quiet", None),
            "{reader}, the carry busy: its relaunch is still to come"
        );
    }
    drop(lock);
    let mut st = load(&o, session).expect("kept");
    assert!(st.seen_gone(now_s()));
    assert!(
        crate::harness::relaunch::expired(&st, now_s()).is_none(),
        "the carry relaunches it"
    );
    save(&o, session, &st);
    for (reader, kind, command) in read() {
        assert_eq!(
            (kind, command),
            ("quiet", None),
            "{reader}, stamped by the carry"
        );
    }
    save(&o, session, &record(now_s() - HUNG_EXIT_AFTER_S - 60));
    for (reader, kind, command) in read() {
        assert_eq!(kind, "agent-down", "{reader}, past the hung-exit bound");
        assert!(
            command
                .as_deref()
                .is_some_and(|c| c.contains(&format!("--resume {session}"))),
            "{reader}: the line that resumes it: {command:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_restarts_agent_the_process_table_says_gone_is_never_said_still_shutting_down() {
    let dir = ScratchDir::new("upgrade-status", "gone-not-exiting");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let gone = {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let session = "0badf00d-1111-2222-3333-444455556666";
    let signalled = now_s() - super::super::STALE_S - 60;
    let read = |pid: u32, off: bool| {
        save(
            &o,
            session,
            &St {
                agent: upgrade::Agent::Claude,
                phase: Phase::Exiting { at_s: signalled },
                from: "2.1.302".to_string(),
                to: "2.1.303".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: signalled - 600,
                pid,
                shell: std::process::id(),
                exited_at: 0,
                wait: "exiting".to_string(),
                looked_at: now_s() - 30,
                ..St::default()
            },
        );
        let tab = [TAB.to_string()];
        let mut view = View::default();
        if off {
            view.set_off(&tab);
        }
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |rows| {
            sent = rows.to_vec();
        });
        let aside = Aside {
            off: if off { tab.into() } else { BTreeSet::new() },
            ..Aside::default()
        };
        let (cli, _) = status_rows_with(&o, now_s(), &nobody, &|| aside.clone());
        [("the window", sent), ("--status", cli)].map(|(reader, rows)| {
            assert_eq!(rows.len(), 1, "{reader}: {rows:?}");
            let quiet = matches!(rows[0].attention(now_s()), Attention::Quiet);
            (reader, rows[0].stall(now_s()), quiet)
        })
    };
    for off in [true, false] {
        for (reader, stall, quiet) in read(gone, off) {
            assert_eq!(
                stall.as_deref(),
                Some("stuck:exited"),
                "{reader}, off={off}: its agent is gone"
            );
            assert!(
                quiet,
                "{reader}, off={off}: raised only at the hung-exit bound"
            );
        }
        for pid in [std::process::id(), 0] {
            for (reader, stall, quiet) in read(pid, off) {
                assert_eq!(
                    stall.as_deref(),
                    Some("stuck:exiting"),
                    "{reader}, off={off}, pid {pid}"
                );
                assert!(quiet, "{reader}, off={off}, pid {pid}");
            }
        }
    }
}

/// A press on a tab whose restart in flight another conversation overtook (its agent gone, the
/// tab's live agent on another conversation) closes that restart (`tab-taken`) and lands on the
/// live conversation's row; while the restart's agent lives, or while the holder read in front is
/// a process that did not write its registry (its recorded birth another's: a crashed agent's
/// file whose number another program took), the press is refused as before.
#[cfg(unix)]
#[test]
fn a_press_closes_a_restart_its_tab_overtook_and_lands() {
    const A: &str = "aaaaaaaa-1111-2222-3333-444455556666";
    const B: &str = "bbbbbbbb-1111-2222-3333-444455556666";
    let dead = {
        let mut child = aterm_uds::spawnfd::command("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let (me, other) = (std::process::id(), std::os::unix::process::parent_id());
    let born = super::super::kernel_start(me).expect("this process's start");
    let leftover = "Mon Jan  1 00:00:00 2001";
    assert_ne!(
        born,
        super::super::squash(leftover),
        "precondition: another process's birth"
    );
    for cause in ["", crate::harness::relaunch::CAUSE_EXIT] {
        for (why, pid, start, closes) in [
            ("its agent gone", dead, born.as_str(), true),
            ("its agent alive", other, born.as_str(), false),
            ("its holder a leftover file's", dead, leftover, false),
        ] {
            let dir = ScratchDir::new("upgrade-status", "press-overtaken");
            let o = opts(&dir);
            std::fs::create_dir_all(state_dir(&o)).expect("state");
            let now = now_s();
            let moving = |phase: Phase, pid: u32, cause: &str| St {
                phase,
                pid,
                shell: other,
                from: "2.1.281".to_string(),
                to: "2.1.282".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: now - 60,
                cause: cause.to_string(),
                ..St::default()
            };
            save(&o, A, &moving(Phase::Exiting { at_s: now - 5 }, pid, cause));
            save(&o, B, &moving(Phase::Pending, me, ""));
            let held = |vet: &BTreeSet<String>| -> Option<Vec<Holder>> {
                assert!(vet.contains(A) && vet.contains(B), "{vet:?}");
                Some(vec![Holder {
                    pid: me,
                    proc_start: start.to_string(),
                    ..holder(B, "2.1.281", Some(TAB))
                }])
            };
            let got = ask_within(&o, TAB, Ask::Press, None, ASK_LOCK_WAIT, &held);
            let a = load(&o, A).expect("A kept");
            let b = load(&o, B).expect("B kept");
            if closes {
                let (row, _) = got.expect("the press lands");
                assert_eq!(row.session, B, "{why} (cause={cause})");
                assert_eq!(
                    (a.phase, a.exited_at),
                    (Phase::Failed("tab-taken".to_string()), 0),
                    "{why} (cause={cause})"
                );
                assert_eq!(b.request, Request::Now, "{why} (cause={cause})");
            } else {
                let e = got.expect_err("refused");
                assert!(
                    e.contains("is already under way"),
                    "{why} (cause={cause}): {e}"
                );
                assert_eq!(
                    a.phase,
                    Phase::Exiting { at_s: now - 5 },
                    "{why} (cause={cause})"
                );
                assert_eq!(b.request, Request::None, "{why} (cause={cause})");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn a_restart_down_in_flight_is_lowered_once_its_conversation_is_live_again() {
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let session = "0badf00d-1111-2222-3333-444455556666";
    let other = "11112222-3333-4444-5555-666677778888";
    let gone = {
        let mut child = aterm_uds::spawnfd::command("/usr/bin/true")
            .spawn()
            .expect("spawn /usr/bin/true");
        let pid = child.id();
        child.wait().expect("reap");
        pid
    };
    let stale = super::super::STALE_S;
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let kinds = [
        (
            "seen gone by a look",
            now_s() - stale - 60,
            now_s() - stale - 30,
            "",
        ),
        (
            "read gone by the process table",
            now_s() - HUNG_EXIT_AFTER_S - 60,
            0,
            "",
        ),
        (
            "a fresh restart",
            now_s() - stale - 60,
            now_s() - stale - 30,
            fresh,
        ),
    ];
    for (n, (what, signalled, exited_at, cause)) in kinds.into_iter().enumerate() {
        let dir = ScratchDir::new("upgrade-status", &format!("down-in-flight-{n}"));
        let o = opts(&dir);
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        save(
            &o,
            session,
            &St {
                agent: upgrade::Agent::Claude,
                phase: Phase::Exiting { at_s: signalled },
                from: "2.1.281".to_string(),
                to: "2.1.283".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: signalled - 600,
                pid: gone,
                exited_at,
                cause: cause.to_string(),
                ..St::default()
            },
        );
        let read = |held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
            let mut view = View::default();
            view.set_off(&[TAB.to_string()]);
            let mut sent: Vec<Row> = Vec::new();
            let _ = view.refresh_with(&o, &tabs, now_s(), held, &mut |rows| sent = rows.to_vec());
            let aside = Aside {
                off: [TAB.to_string()].into(),
                ..Aside::default()
            };
            let (cli, vetted) = status_rows_with(&o, now_s(), held, &|| aside.clone());
            assert!(vetted);
            [("the window", sent), ("--status", cli)]
        };
        let raised = |rows: &[Row]| -> Vec<(&'static str, Option<String>)> {
            rows.iter()
                .filter_map(|r| match r.attention(now_s()) {
                    Attention::Interrupt(need) => Some((need.kind, need.command)),
                    _ => None,
                })
                .collect()
        };
        let held_by = |who: Vec<(&'static str, u32, &'static str)>| {
            move |_: &BTreeSet<String>| -> Option<Vec<Holder>> {
                Some(
                    who.iter()
                        .map(|(s, pid, version)| Holder {
                            pid: *pid,
                            led: true,
                            ..holder(s, version, Some(TAB))
                        })
                        .collect(),
                )
            }
        };
        let down = |rows: &[Row], why: &str| {
            let said = raised(rows);
            assert!(
                matches!(said.as_slice(), [("agent-down", Some(_))]),
                "{what}, {why}: agent-down with its line: {said:?}"
            );
        };
        for (reader, rows) in read(&held_by(Vec::new())) {
            down(&rows, &format!("{reader}, nobody"));
        }
        for (reader, rows) in read(&held_by(vec![(session, gone, "2.1.281")])) {
            down(&rows, &format!("{reader}, the ended agent itself"));
        }
        let resumed = if cause == fresh { other } else { session };
        let me = std::process::id();
        for (reader, rows) in read(&held_by(vec![(resumed, me, "2.1.281")])) {
            assert_eq!(raised(&rows), Vec::new(), "{what}, {reader}: live again");
            if cause == fresh {
                assert!(rows.is_empty(), "{what}, {reader}: {rows:?}");
            } else {
                assert_eq!(rows.len(), 1, "{what}, {reader}: a behind holder");
                assert_eq!(
                    rows[0].attention(now_s()),
                    Attention::Quiet,
                    "{what}, {reader}"
                );
            }
        }
        for (reader, rows) in read(&held_by(vec![(resumed, me, "2.1.283")])) {
            assert!(rows.is_empty(), "{what}, {reader}, current: {rows:?}");
        }
        let [(_, window), _] = read(&held_by(Vec::new()));
        assert_eq!(raised(&window), Vec::new(), "{what}: quit by hand");
    }
}

#[test]
fn a_record_with_no_recorded_tab_reads_the_claim_on_its_holders_tab() {
    let dir = ScratchDir::new("upgrade-status", "claimed-untabbed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        pending_since: NOW - 7 * 3_600,
        looked_at: NOW - 3 * 3_600,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW - 7 * 3_600);
    pin_awake_since(NOW - 86_400);
    let cli = |claimed: &[&str]| {
        let claimed: BTreeSet<String> = claimed.iter().map(|t| (*t).to_string()).collect();
        let (mut rows, vetted) = status_rows_with(&o, NOW, &behind_in(TAB), &|| claims(&claimed));
        assert!(vetted);
        rows.remove(0)
    };
    let row = cli(&[TAB]);
    assert_eq!(row.tab, TAB, "shown under its holder's tab");
    assert_eq!((row.aside.as_str(), row.wait.as_str()), (CLAIMED, CLAIMED));
    assert_eq!(row.unwatched(NOW), None);
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    for claimed in [&[][..], &["s-another"][..]] {
        assert_eq!(
            cli(claimed).stall(NOW).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {claimed:?}"
        );
    }
    pin_awake_since(0);
}

#[test]
fn a_record_with_no_recorded_tab_is_watched_in_its_holders_tab() {
    let dir = ScratchDir::new("upgrade-status", "watch-untabbed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: String::new(),
        pending_since: NOW - 60,
        ..St::default()
    };
    super::super::save_at(&o, "old-build", &st, NOW);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let due = NOW + upgrade::REASK_S;
    let mut view = View::default();
    assert_eq!(
        view.refresh_with(&o, &tabs, NOW, &behind_in(TAB), &mut |_| {}),
        Some(due),
        "its watch is an instant the host wakes for"
    );
    assert!(view.watch_due().is_empty(), "not yet due");
    let _ = view.refresh_with(&o, &tabs, due, &behind_in(TAB), &mut |_| {});
    assert_eq!(view.watch_due(), [TAB], "due, under its holder's tab");

    let mut elsewhere = View::default();
    assert_eq!(
        elsewhere.refresh_with(&o, &tabs, NOW, &behind_in("s-elsewhere"), &mut |_| {}),
        None,
        "held in a tab not shown here: no instant"
    );
    let _ = elsewhere.refresh_with(&o, &tabs, due, &behind_in("s-elsewhere"), &mut |_| {});
    assert!(elsewhere.watch_due().is_empty(), "nor listed");
    let _ = elsewhere.refresh_with(&o, &tabs, due, &|_| None, &mut |_| {});
    assert!(
        elsewhere.watch_due().is_empty(),
        "holders unread: no tab proven"
    );
}

#[test]
fn the_view_names_the_instant_a_person_stops_being_named() {
    let flips = NOW - 10 + ATTENDED_SEEN_S + 1;
    let attended = Row {
        wait: "attended".to_string(),
        wait_since: NOW - 600,
        wait_seen: NOW - 10,
        ..pending(7 * 3_600)
    };
    assert_eq!(
        next_change(std::slice::from_ref(&attended), NOW),
        Some(flips)
    );
    assert!(attended.person_seen(flips - 1));
    assert!(!attended.person_seen(flips));
    assert_eq!(next_change(std::slice::from_ref(&attended), flips), None);
    let codex = Row {
        agent: upgrade::Agent::Codex,
        ..attended.clone()
    };
    assert_eq!(next_change(std::slice::from_ref(&codex), NOW), None);
    assert!(!codex.person_seen(NOW));

    let st = St {
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 7 * 3_600,
        wait: "attended".to_string(),
        wait_since: NOW - 600,
        wait_seen: NOW - 10,
        ..St::default()
    };
    let seen = Row::of("c", &st, flips - 1);
    let unseen = Row::of("c", &st, flips);
    assert!(seen.person_named && !unseen.person_named);
    assert_ne!(seen, unseen, "a changed row, handed over again");
    assert_eq!(
        Row {
            person_named: false,
            ..seen.clone()
        },
        unseen,
        "the flag is the whole change"
    );
    assert_eq!(
        seen,
        Row::of("c", &st, NOW),
        "before the flip, the same row"
    );
}

#[test]
fn the_host_sends_its_tabs_rows_only_when_they_change() {
    let dir = ScratchDir::new("upgrade-status", "view");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 60,
        ..St::default()
    };
    save(&o, "aaa", &st);
    save(
        &o,
        "zzz",
        &St {
            tab: "s-elsewhere".to_string(),
            ..st.clone()
        },
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let mut view = View::default();
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let held = behind_in(TAB);
    let now = now_s();
    view.refresh_with(&o, &tabs, now, &held, &mut |rows| {
        sent.push(rows.to_vec());
    });
    view.refresh_with(&o, &tabs, now + 60, &held, &mut |rows| {
        sent.push(rows.to_vec());
    });
    assert_eq!(sent.len(), 1, "an unchanged upgrade is not sent again");
    assert_eq!(sent[0].len(), 1, "only this instance's live tabs");
    assert_eq!(sent[0][0].tab, TAB);
    let before = now_s();
    let again = view.refresh(&o, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 1, "an unreadable roster says nothing");
    assert!(
        again.is_some_and(|t| t >= before + 2 && t <= now_s() + 2),
        "{again:?}"
    );
    let later = view.refresh(&o, &mut |rows| sent.push(rows.to_vec()));
    assert!(
        later.is_some_and(|t| t >= before + 16),
        "a growing pause: {later:?}"
    );
    view.refresh_with(&o, &tabs, now + 60, &|_| None, &mut |rows| {
        sent.push(rows.to_vec());
    });
    assert_eq!(sent.len(), 1, "unreadable session files say nothing");
    save(
        &o,
        "aaa",
        &St {
            pending_since: now_s() - STALLED_AFTER_S,
            ..st.clone()
        },
    );
    view.refresh_with(&o, &tabs, now_s(), &held, &mut |rows| {
        sent.push(rows.to_vec());
    });
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1][0].stalled.as_deref(), Some("overdue"));
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 3);
    assert!(sent[2].is_empty(), "standing down hands over no rows");
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 3, "said once");
}

#[test]
fn a_completed_read_reoffers_unchanged_rows_when_the_publication_source_changes() {
    let dir = ScratchDir::new("upgrade-status", "reoffer");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Pending,
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            tab: TAB.into(),
            pending_since: NOW - 60,
            ..St::default()
        },
    );
    let tabs = [LiveTab {
        sid: TAB.into(),
        fgpgid: Some(7),
    }];
    let mut view = View::default();
    let mut sent = Vec::new();
    let held = behind_in(TAB);
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 1);
    view.reoffer_after_read();
    view.refresh_with(&o, &tabs, NOW, &|_| None, &mut |rows| {
        sent.push(rows.to_vec())
    });
    assert_eq!(
        sent.len(),
        1,
        "a failed holder read cannot re-stamp cached rows"
    );
    view.refresh(&o, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(
        sent.len(),
        1,
        "a failed roster read cannot re-stamp cached rows"
    );
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(
        sent.len(),
        2,
        "the next completed read reoffers the unchanged row"
    );
    assert_eq!(sent[0], sent[1]);
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 2, "the new source is deduplicated again");
    view.reoffer_after_read();
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(
        sent.len(),
        3,
        "a pending reoffer must not lose the stand-down clear"
    );
    assert!(sent[2].is_empty());
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(
        sent.len(),
        3,
        "an unchanged stood-down source says nothing again"
    );
    view.reoffer_after_read();
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(
        sent.len(),
        4,
        "a changed source forces a new clear after sent was reset"
    );
    assert!(sent[3].is_empty());
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 4, "that fresh clear is deduplicated again");
}

#[test]
fn pending_rows_keep_the_records_run_identity_without_a_recorded_pid() {
    use super::super::Proc;

    let from = Version::parse("2.1.281").unwrap();
    let to = upgrade::Candidate {
        exe: "/candidate/claude".into(),
        version: Version::parse("2.1.282").unwrap(),
        source: upgrade::Source::Managed,
    };
    let old = St::fresh(
        &from,
        &to,
        Proc {
            start: "Mon Oct 26 12:00:00 2026",
            started_s: NOW,
        },
        NOW,
    );
    let new = St::fresh(
        &from,
        &to,
        Proc {
            start: "Mon Oct 26 12:01:00 2026",
            started_s: NOW,
        },
        NOW,
    );
    assert_eq!((old.pid, new.pid), (0, 0));
    let before = Row::of("same-conversation", &old, NOW);
    let after = Row::of("same-conversation", &new, NOW);
    assert_eq!(before.proc_start, old.proc_start);
    assert_eq!(after.proc_start, new.proc_start);
    assert_ne!(
        before, after,
        "the real St::fresh producer distinguishes same-build runs"
    );
    assert_eq!(
        Row {
            proc_start: before.proc_start.clone(),
            ..after
        },
        before,
        "the birth field is the distinction, without inventing a PID"
    );
}

#[test]
fn a_first_holder_names_the_pending_records_run() {
    const A: &str = "Mon Oct 26 12:00:00 2026";
    const B: &str = "Mon Oct 26 12:01:00 2026";
    for phase in [
        Phase::Pending,
        Phase::Announced { at_s: NOW, asks: 1 },
        Phase::Failed(upgrade::GAVE_UP.into()),
    ] {
        for (record, current, stands) in [
            (A, A, true),
            (B, B, true),
            ("", B, true),
            (A, "", true),
            ("", "", true),
            (A, "Mon  Oct 26 12:00:00 2026", true),
            (A, B, false),
        ] {
            let (cli, window) = first_holder_rows(record, current, phase.clone(), 0);
            assert_eq!(
                cli.len(),
                usize::from(stands),
                "CLI: record={record:?}, current={current:?}"
            );
            assert_eq!(
                window.len(),
                usize::from(stands),
                "window: record={record:?}, current={current:?}"
            );
            for row in cli.iter().chain(&window) {
                assert_eq!(row.pid, 0, "real pending St::fresh records no PID");
                assert_eq!(
                    row.proc_start, record,
                    "a current holder cannot rewrite the record's birth"
                );
            }
        }
    }
}

#[test]
fn a_first_holder_can_answer_a_down_record_from_the_previous_run() {
    const A: &str = "Mon Oct 26 12:00:00 2026";
    const B: &str = "Mon Oct 26 12:01:00 2026";
    for phase in [
        Phase::Exiting { at_s: NOW },
        Phase::Failed("no-resume".into()),
    ] {
        let (cli, window) = first_holder_rows(A, B, phase.clone(), NOW);
        assert_eq!(
            (cli.len(), window.len()),
            (1, 1),
            "{phase:?}: a live resumption answers the ended run"
        );
        assert!(
            cli.iter().chain(&window).all(|row| !row.down_after_exit()),
            "{phase:?}: intentional held-again semantics remain"
        );
    }
}

/// The actual first publication: mint and save the pending record, project the current
/// SessionFile through holder_of, then run both CLI and window status paths from empty views.
fn first_holder_rows(
    record: &str,
    current: &str,
    phase: Phase,
    exited_at: u64,
) -> (Vec<Row>, Vec<Row>) {
    use super::super::Proc;
    let dir = ScratchDir::new("upgrade-status", "first-holder");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("private records");
    let from = Version::parse("2.1.281").unwrap();
    let target = upgrade::Candidate {
        exe: "/candidate/claude".into(),
        version: Version::parse("2.1.282").unwrap(),
        source: upgrade::Source::Managed,
    };
    let mut st = St::fresh(
        &from,
        &target,
        Proc {
            start: record,
            started_s: NOW,
        },
        NOW,
    );
    st.tab = TAB.into();
    st.phase = phase;
    st.exited_at = exited_at;
    save(&o, "same-conversation", &st);
    let saved = load(&o, "same-conversation").expect("the actual saved record");
    assert_eq!(saved.proc_start, record);
    assert_eq!(saved.pid, 0);
    let session = upgrade::SessionFile {
        pid: std::process::id(),
        session_id: "same-conversation".into(),
        cwd: "/".into(),
        version: from.to_string(),
        status: "idle".into(),
        status_updated_at_ms: NOW * 1_000,
        proc_start: current.into(),
        kind: "interactive".into(),
        entrypoint: "cli".into(),
        claude_dir: None,
        profile: None,
    };
    let mut holder = holder_of(&session, None);
    // The private roster supplies the tab/foreground proof; holder_of supplies the record data.
    holder.tab = Some(TAB.into());
    holder.led = true;
    holder.aside = false;
    let held = |_: &BTreeSet<String>| Some(vec![holder.clone()]);
    let (cli, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert!(whole);
    let mut view = View::default();
    let mut window = None;
    let tabs = [LiveTab {
        sid: TAB.into(),
        fgpgid: Some(i64::from(holder.pid)),
    }];
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| {
        window = Some(rows.to_vec())
    });
    (
        cli,
        window.expect("the first coherent view always publishes"),
    )
}

#[test]
fn special_and_oversized_records_keep_the_tab_without_blocking_the_reader() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-bounded-records-")
        .tempdir()
        .expect("private record directory");
    let o = opts(dir.path());
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        home: Some(o.home.to_string_lossy().into_owned()),
        moved_at: NOW,
        pending_since: NOW - STALLED_AFTER_S - 60,
        ..St::default()
    };
    save(&o, "aaa", &st);
    let record = state_dir(&o).join("aaa.json");
    let moved = state_dir(&o).join("aaa.move");
    let claude = o.home.join(".claude");
    std::fs::create_dir_all(claude.join("sessions")).expect("private session records");
    let roster = claude.join(format!("sessions/{}.json", std::process::id()));
    let ignored = state_dir(&o).join("a-directory.json");
    std::fs::create_dir(&ignored).expect("a directory is not a record");
    let saved = st.to_json();

    for fault in ["oversized", "fifo", "linked-fifo", "dangling"] {
        std::fs::write(&record, &saved).expect("the readable record");
        let mut view = View::default();
        view.refresh_with(&o, &tabs, NOW, &behind_in(TAB), &mut |rows| {
            assert_eq!(
                rows.len(),
                1,
                "a readable record beside an ignored directory"
            );
        });
        let (rows, whole) = read_rows(&o, NOW);
        assert_eq!(rows.len(), 1);
        assert!(whole);

        let target = dir.path().join("fifo-target");
        let fifo = |path: &Path| {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
            // SAFETY: the private NUL-terminated path outlives this call.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
        };
        if fault == "linked-fifo" {
            fifo(&target);
        }
        let mut oversized = saved.as_bytes().to_vec();
        oversized.resize(2 * 1024 * 1024 + 1, b' ');
        let paths = [&record, &moved, &roster];
        for path in paths {
            let _ = std::fs::remove_file(path);
            match fault {
                "oversized" => std::fs::write(path, &oversized).expect("oversized valid JSON"),
                "fifo" => fifo(path),
                _ => std::os::unix::fs::symlink(&target, path).expect("linked record"),
            }
        }
        let identities: Vec<_> = paths
            .iter()
            .map(|p| {
                let m = std::fs::symlink_metadata(p).expect("record identity");
                (m.dev(), m.ino())
            })
            .collect();
        let read_opts = o.clone();
        let read_tabs = tabs.clone();
        let write = st.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let (rows, whole) = read_rows(&read_opts, NOW);
            assert!(rows.is_empty());
            assert!(
                !whole,
                "a listed record that cannot be read is not an all-clear"
            );
            view.refresh_with_keeping(
                &read_opts,
                &read_tabs,
                NOW,
                &behind_in(TAB),
                &mut |rows, kept| {
                    assert!(rows.is_empty());
                    assert_eq!(kept, [TAB]);
                },
            );
            assert_eq!(view.unreadable(), [state_dir(&read_opts).join("aaa.json")]);
            assert!(load(&read_opts, "aaa").is_none());
            assert!(super::super::kept_move(&read_opts, "aaa").is_none());
            assert!(super::super::recorded_homes(&read_opts).is_empty());
            assert!(!super::super::confirming(&read_opts));
            assert!(
                super::super::session_file_in(&read_opts.home.join(".claude"), std::process::id())
                    .is_none()
            );
            assert!(super::super::session_files_in(&read_opts.home.join(".claude")).is_none());
            assert!(super::super::save_checked(&read_opts, "aaa", &write).is_err());
            send.send(view).expect("reader result");
        });
        let mut view = receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the record reader must not block");
        reader.join().expect("reader");
        for (path, before) in paths.into_iter().zip(identities) {
            let after = std::fs::symlink_metadata(path).expect("record remains");
            assert_eq!(
                (after.dev(), after.ino()),
                before,
                "unread state was not replaced"
            );
            if fault == "oversized" {
                assert_eq!(std::fs::read(path).expect("preserved bytes"), oversized);
            }
            std::fs::remove_file(path).expect("private record");
        }
        if fault == "linked-fifo" {
            std::fs::remove_file(&target).expect("private FIFO target");
        }
        std::fs::write(&record, &saved).expect("repair the record");
        view.refresh_with_keeping(&o, &tabs, NOW, &behind_in(TAB), &mut |rows, kept| {
            assert_eq!(rows.len(), 1, "{fault}: repair restores the row");
            assert!(kept.is_empty());
        });
        assert!(view.unreadable().is_empty());
        super::super::save_checked(&o, "aaa", &st).expect("a readable state can be saved");
    }
}

#[test]
fn planted_temporary_records_do_not_block_or_redirect_a_save() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-record-temp-")
        .tempdir()
        .expect("private record directory");
    let o = opts(dir.path());
    let record = state_dir(&o).join("aaa.json");
    let moved = state_dir(&o).join("aaa.move");
    let temps = [
        record.with_extension("json.tmp"),
        moved.with_extension("move.tmp"),
    ];
    let sentinel = dir.path().join("elsewhere");
    let sentinel_bytes = b"a private sentinel\n";
    std::fs::write(&sentinel, sentinel_bytes).expect("sentinel");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        moved_at: NOW,
        moved_login: Some("kept-login".to_string()),
        ..St::default()
    };

    for (round, poison) in ["fifo", "symlink"].into_iter().enumerate() {
        if round != 0 {
            std::fs::remove_file(&moved).expect("previous move record");
        }
        save(&o, "aaa", &st);
        assert!(load(&o, "aaa").is_some(), "seed record");
        for path in &temps {
            if poison == "fifo" {
                let name =
                    std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
                // SAFETY: the private NUL-terminated path outlives this call.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            } else {
                std::os::unix::fs::symlink(&sentinel, path).expect("planted link");
            }
        }
        let identities: Vec<_> = temps
            .iter()
            .map(|path| {
                let m = std::fs::symlink_metadata(path).expect("temporary identity");
                (m.dev(), m.ino())
            })
            .collect();
        let read_opts = o.clone();
        let write = st.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            let mut ordinary = write.clone();
            ordinary.wait = "ordinary-save".to_string();
            save(&read_opts, "aaa", &ordinary);
            assert_eq!(
                load(&read_opts, "aaa").expect("saved record").wait,
                "ordinary-save"
            );

            let mut restored = write;
            restored.wait = "restored".to_string();
            super::super::restore(&read_opts, "aaa", Some(&restored));
            assert_eq!(
                load(&read_opts, "aaa").expect("restored record").wait,
                "restored"
            );

            super::super::remove(&read_opts, "aaa");
            assert!(load(&read_opts, "aaa").is_none(), "current record removed");
            let kept = super::super::kept_move(&read_opts, "aaa").expect("move retained");
            assert_eq!(kept.moved_at, NOW);
            assert_eq!(kept.moved_login.as_deref(), Some("kept-login"));
            send.send(()).expect("writer result");
        });
        receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("record replacements must not open a planted temporary");
        writer.join().expect("writer");
        assert_eq!(std::fs::read(&sentinel).expect("sentinel"), sentinel_bytes);
        for (path, before) in temps.iter().zip(identities) {
            let after = std::fs::symlink_metadata(path).expect("temporary remains");
            assert_eq!(
                (after.dev(), after.ino()),
                before,
                "planted temporary untouched"
            );
            std::fs::remove_file(path).expect("private temporary");
        }
    }
}

#[test]
fn records_that_cannot_be_read_whole_hand_the_window_nothing() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-upgrade-unreadable-records-")
        .tempdir()
        .expect("temp dir");
    let o = opts(dir.path());
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut kept: Vec<Vec<String>> = Vec::new();
    let mut view = View::default();
    std::fs::create_dir_all(&o.state).expect("state");
    std::fs::write(state_dir(&o), b"").expect("a file in the directory's place");
    let next = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert!(
        sent.is_empty(),
        "an unlistable state directory hands over nothing"
    );
    assert_eq!(next, None);
    assert!(rows_at(&o, NOW).is_empty(), "--status reads no records");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert!(status.is_empty());
    assert!(
        !whole,
        "no rows from an unlistable directory is not an all-clear"
    );
    assert_eq!(
        view.unlisted(),
        Some(state_dir(&o).as_path()),
        "the directory it cannot list is named"
    );
    std::fs::remove_file(state_dir(&o)).expect("unblock");
    std::fs::create_dir_all(state_dir(&o)).expect("state dir");
    crate::harness::upgrade_drive::set_models(&o, "claude-opus-5-5")
        .expect("a model priority cache beside the records");
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: NOW - STALLED_AFTER_S - 60,
            ..St::default()
        },
    );
    let looped = state_dir(&o).join("bbb.json");
    std::os::unix::fs::symlink(&looped, &looped).expect("a record that loops");
    view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, tabs| {
        sent.push(rows.to_vec());
        kept.push(tabs.to_vec());
    });
    assert_eq!(sent.len(), 1, "the rows that read are handed over");
    assert_eq!(sent[0].len(), 1);
    assert_eq!(view.unlisted(), None, "listed whole: no directory named");
    assert_eq!(
        kept,
        [vec![TAB.to_string()]],
        "with its conversation's tab left as it stands: the record names none it can be read for"
    );
    assert_eq!(
        view.unreadable(),
        std::slice::from_ref(&looped),
        "the record it cannot read is named"
    );
    assert_eq!(
        rows_at(&o, NOW).len(),
        1,
        "--status reads the one that reads"
    );
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert_eq!(status.len(), 1, "the readable sibling is still shown");
    assert_eq!(status[0].session, "aaa");
    assert!(
        !whole,
        "a record that could not be read leaves status incomplete"
    );
    std::fs::remove_file(&looped).expect("unloop");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert_eq!(status.len(), 1);
    assert!(whole, "readable records and holders make status complete");
    view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, tabs| {
        sent.push(rows.to_vec());
        kept.push(tabs.to_vec());
    });
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].len(), 1);
    assert_eq!(sent[1][0].stalled.as_deref(), Some("overdue"));
    assert!(kept[1].is_empty(), "read whole: no tab left as it stands");
    assert!(view.unreadable().is_empty(), "read whole: none named");

    let record = state_dir(&o).join("aaa.json");
    let saved = std::fs::read_to_string(&record).expect("the live upgrade record");
    for malformed in [
        "{",
        "null",
        "[]",
        r#"{"phase":"future"}"#,
        r#"{"phase":"future","priority":["claude-opus-5-5"]}"#,
    ] {
        std::fs::write(&record, malformed).expect("a readable malformed record");
        let mut frames = Vec::new();
        view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, tabs| {
            frames.push((rows.to_vec(), tabs.to_vec()));
        });
        assert_eq!(frames.len(), 1);
        assert!(frames[0].0.is_empty());
        assert_eq!(frames[0].1, [TAB.to_string()]);
        assert_eq!(view.unreadable(), std::slice::from_ref(&record));
        let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
        assert!(status.is_empty());
        assert!(!whole, "malformed bytes cannot report no upgrade");

        std::fs::write(&record, &saved).expect("restore the live upgrade");
        let mut restored = Vec::new();
        view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, tabs| {
            restored.push((rows.to_vec(), tabs.to_vec()));
        });
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].0.len(), 1);
        assert_eq!(restored[0].0[0].session, "aaa");
        assert!(restored[0].1.is_empty());
        assert!(view.unreadable().is_empty());
    }

    let mut other = St::from_json(&saved).expect("the saved record");
    other.cause = crate::harness::relaunch::CAUSE_EXIT.to_string();
    std::fs::write(&record, other.to_json()).expect("a valid non-upgrade record");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert!(status.is_empty());
    assert!(whole, "a valid non-upgrade is intentionally omitted");

    std::fs::write(&record, &saved).expect("restore the live upgrade");
    let filtered = Opts {
        only_sid: Some("s-other".to_string()),
        ..o.clone()
    };
    let (status, whole) = read_rows(&filtered, NOW);
    assert!(status.is_empty());
    assert!(whole, "a valid record for another tab is omitted");

    // A retired writer's state document (the shape m22 kept from 2026-09-24) shares the
    // folder: it is no upgrade record, so it neither hides the live row nor makes the
    // status incomplete, while the damaged-record cases above stay unreadable.
    let stray = state_dir(&o).join("state.json");
    std::fs::write(
        &stray,
        r#"{"acts":[],"badges":[],"conversations":{},"last_act_at":1790259448}"#,
    )
    .expect("a stray state document");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert_eq!(status.len(), 1, "the live upgrade beside a stray document");
    assert!(
        whole,
        "a document with no phase is not an unreadable record"
    );
    let mut frames = Vec::new();
    view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, tabs| {
        frames.push((rows.to_vec(), tabs.to_vec()));
    });
    assert!(
        frames.is_empty(),
        "the stray document changes nothing the window holds: {frames:?}"
    );
    assert!(
        view.unreadable().is_empty(),
        "the stray document is not named"
    );
    // A restart stamp (`asked-<tab>.json`, no phase) is no record either.
    let stamp = state_dir(&o).join(format!("asked-{TAB}.json"));
    std::fs::write(
        &stamp,
        r#"{"pid":4242,"start":1790259000,"cause":"exit","at":1790259448}"#,
    )
    .expect("a restart stamp");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert_eq!(status.len(), 1, "the live upgrade beside a restart stamp");
    assert!(whole, "a restart stamp is not an unreadable record");
    // But a phase-less object under a record's own name is that record, damaged.
    for name in ["0badf00d-1111-4222-8333-444455556666", "codex-s-damaged"] {
        let damaged = state_dir(&o).join(format!("{name}.json"));
        std::fs::write(&damaged, "{}").expect("a record emptied to an object");
        let (_, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
        assert!(!whole, "{name}: an emptied record stays unreadable");
        let (_, _, unread) = read_records(&o, NOW);
        assert_eq!(
            unread.iter().map(|u| u.path.clone()).collect::<Vec<_>>(),
            std::slice::from_ref(&damaged),
            "{name}: the emptied record is the one named"
        );
        std::fs::remove_file(&damaged).expect("drop the emptied record");
    }
    std::fs::remove_file(&stamp).expect("drop the restart stamp");
    std::fs::remove_file(&stray).expect("drop the stray document");
    std::fs::remove_dir_all(state_dir(&o)).expect("no records");
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 3);
    assert!(sent[2].is_empty(), "no directory: no records, handed over");
    assert_eq!(view.unlisted(), None, "no directory: none named");
    let (status, whole) = status_rows_with(&o, NOW, &held, &Aside::default);
    assert!(status.is_empty());
    assert!(whole, "a missing directory means there are no records");
    std::fs::write(state_dir(&o), b"").expect("a file in the directory's place again");
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert!(view.unlisted().is_some());
    view.stand_down(&mut |rows| sent.push(rows.to_vec()));
    assert_eq!(view.unlisted(), None, "stood down: none named");
}

#[test]
fn a_state_file_from_either_older_writer_is_read_with_its_missing_fields_at_default() {
    let before = r#"{"phase":"announced","why":"","from":"2.1.281","to":"2.1.282",
        "source":"managed","marker":"ATERM-UPGRADE-READY-1","notice_start":"x","tab":"s-1",
        "line":"","noted":"","model_before":"","launch_model":"","resumed_on":"",
        "at":90,"asks":2,"salt":7,"last_seq":3,"seq_since":80,"pid":11,"notice_pid":11,
        "shell":10,"mark":0,"confirm_by":0,"resumed_pid":0}"#;
    let st = St::from_json(before).expect("the older build's file");
    assert_eq!(st.phase, Phase::Announced { at_s: 90, asks: 2 });
    assert_eq!((st.request.clone(), st.wait.as_str()), (Request::None, ""));
    assert_eq!(st.behind_since(), 7, "no pending_since: the salt");
    let unknown = before.replace(r#""asks":2,"#, r#""asks":2,"told":1,"ready":1,"#);
    let st = St::from_json(&unknown).expect("an unknown field's file");
    assert_eq!(st.phase, Phase::Announced { at_s: 90, asks: 2 });
    assert_eq!(st.request, Request::None);
    let owners = before.replace(
        r#""asks":2,"#,
        r#""asks":2,"pending_since":5,"wait":"settling","wait_since":6,"request":"now","request_tab":"s-1","outcome":"","done_at":0,"#,
    );
    let st = St::from_json(&owners).expect("the owner's-view build's file");
    assert!(matches!(st.phase, Phase::Announced { .. }));
    assert_eq!(
        (st.request.clone(), st.wait.as_str(), st.behind_since()),
        (Request::Now, "settling", 5)
    );
    assert_eq!(st.request_at, 0, "no word time: an older writer's");
    let board = owners.replace(
        r#""asks":2,"#,
        r#""asks":2,"yielded":1,"yielded_until":270,"#,
    );
    let st = St::from_json(&board).expect("a board-era file");
    assert_eq!(
        Row::of("c", &st, 100),
        Row::of("c", &St::from_json(&owners).unwrap(), 100)
    );
    let both = St {
        phase: Phase::Announced { at_s: 90, asks: 2 },
        request: Request::DeferUntil(700),
        request_tab: "s-1".to_string(),
        request_at: 88,
        outcome: "claude restarted on 2.1.282 · model x".to_string(),
        done_at: 800,
        ..st
    };
    assert_eq!(St::from_json(&both.to_json()), Some(both.clone()));
    let unknown = both.to_json().replace("defer-until:700", "someday");
    assert_eq!(
        St::from_json(&unknown).expect("still read").request,
        Request::None,
        "an unreadable request asks for nothing"
    );
}

#[cfg(unix)]
fn recording_socket(dir: &std::path::Path) -> (String, crate::harness::FakeInstanceLog) {
    crate::harness::fake_instance(&dir.join("t.sock"), "0badf00d0badf00d", |_| {
        "OK".to_string()
    })
}

#[cfg(unix)]
#[test]
fn only_a_stalled_tab_is_marked_and_its_mark_is_lowered_when_it_recovers() {
    let dir = ScratchDir::new("upgrade-status", "mark");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let waiting = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 60,
        ..St::default()
    };
    let stalled = St {
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: now_s() - 600,
        ..waiting.clone()
    };
    save(&o, "aaa", &waiting);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let meta = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
            .unwrap_or_default()
    };
    let mut view = View::default();
    let held = behind_in(TAB);
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let sweep = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    };
    let sweep_after_exit = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    };
    sweep(&mut view);
    assert_eq!(
        meta(&asked),
        [format!("@{TAB} meta unset attention owner=upgrade")],
        "waiting is never marked"
    );
    asked.lock().expect("log").clear();
    sweep(&mut view);
    assert!(meta(&asked).is_empty(), "and the sweep is once");
    save(&o, "aaa", &stalled);
    sweep_after_exit(&mut view);
    sweep_after_exit(&mut view);
    let marked = meta(&asked);
    assert_eq!(marked.len(), 1, "raised once: {marked:?}");
    assert!(
        marked[0].starts_with(&format!(
            "@{TAB} meta set attention owner=upgrade Claude Code"
        )),
        "{marked:?}"
    );
    assert!(
        marked[0].ends_with("claude --resume aaa"),
        "the mark carries the one action: {marked:?}"
    );
    save(
        &o,
        "aaa",
        &St {
            request: Request::Skip("2.1.282".to_string()),
            ..stalled.clone()
        },
    );
    sweep_after_exit(&mut view);
    let marked = meta(&asked);
    assert_eq!(
        marked.last().map(String::as_str),
        Some(format!("@{TAB} meta unset attention owner=upgrade").as_str()),
        "the owner's skip ends the stall: {marked:?}"
    );
    view.stand_down(&mut |_| {});
    asked.lock().expect("log").clear();
    sweep_after_exit(&mut view);
    assert_eq!(
        meta(&asked),
        [format!("@{TAB} meta unset attention owner=upgrade")],
        "settled again after standing down"
    );
    save(&o, "aaa", &stalled);
    asked.lock().expect("log").clear();
    sweep_after_exit(&mut View::default());
    let said = meta(&asked);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with(&format!("@{TAB} meta set attention owner=upgrade")),
        "{said:?}"
    );
}

#[test]
fn a_row_stands_only_while_its_conversation_is_live_and_behind_in_its_tab() {
    let row = pending(7 * 3_600);
    let s = row.session.as_str();
    assert!(
        row.standing(&[holder(s, "2.1.281", Some(TAB))], false),
        "live, behind, in its tab"
    );
    for (why, held) in [
        ("the conversation ended", vec![]),
        (
            "moved onto the target by hand",
            vec![holder(s, "2.1.282", Some(TAB))],
        ),
        (
            "moved past the target",
            vec![holder(s, "2.1.283", Some(TAB))],
        ),
        (
            "resumed in another tab",
            vec![holder(s, "2.1.281", Some("s-4906566e7ab0a0c15ee3"))],
        ),
        (
            "another conversation in the tab",
            vec![holder("another", "2.1.281", Some(TAB))],
        ),
        (
            "a build nobody can read",
            vec![holder(s, "not-a-version", Some(TAB))],
        ),
    ] {
        assert!(!row.standing(&held, false), "{why}");
        assert!(!row.standing(&held, true), "{why} (the CLI)");
    }
    let unproven = [holder(s, "2.1.281", None)];
    assert!(!row.standing(&unproven, false), "the host proves the tab");
    assert!(row.standing(&unproven, true), "the CLI cannot disprove it");
    let model = Row {
        from: "2.1.282".to_string(),
        ..row.clone()
    };
    assert!(
        model.standing(&[holder(s, "2.1.282", Some(TAB))], false),
        "a model restart on the build it runs"
    );
    assert!(!model.standing(&[holder(s, "2.1.283", Some(TAB))], false));
    assert!(!model.standing(
        &[holder(s, "2.1.282", Some("s-4906566e7ab0a0c15ee3"))],
        false
    ));
    for phase in [
        Phase::Announced { at_s: NOW, asks: 1 },
        Phase::Failed("unanswered".to_string()),
    ] {
        let r = Row {
            phase,
            ..row.clone()
        };
        assert!(!r.standing(&[], false), "{:?} with no holder", r.phase);
        assert!(r.standing(&[holder(s, "2.1.281", Some(TAB))], false));
    }
    for phase in [
        Phase::Exiting { at_s: NOW },
        Phase::Relaunched { at_s: NOW },
        Phase::Done,
    ] {
        let r = Row {
            phase,
            ..row.clone()
        };
        assert!(
            r.standing(&[], false),
            "{:?} is a move, not a wait",
            r.phase
        );
    }
}

#[test]
fn every_owners_word_arms_a_new_round() {
    let dir = ScratchDir::new("upgrade-status", "round");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let to = Version::parse("2.1.282").expect("v");
    let salt = 1_000;
    let announced = St {
        phase: Phase::Announced {
            at_s: now_s() - 60,
            asks: 1,
        },
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        marker: upgrade::ready_marker("aaa", &to, salt + 1),
        salt,
        ..St::default()
    };
    save(&o, "aaa", &announced);
    let asked_from = now_s();
    ask(&o, TAB, Ask::Defer(600), false).expect("defer");
    let asked_by = now_s();
    let held = load(&o, "aaa").expect("state");
    assert_eq!(held.phase, Phase::Pending, "the hold ends the notice");
    assert!(held.marker.is_empty(), "and its marker");
    assert!(held.markers.is_empty(), "every marker of its round");
    assert_eq!(held.release, "deferred");
    assert!(held.salt > salt, "the re-armed notice is a new round");
    assert_eq!(held.behind_since(), salt, "behind since the first round");
    for old in 1..=upgrade::MAX_ASKS {
        for new in 1..=upgrade::MAX_ASKS {
            assert_ne!(
                upgrade::ready_marker("aaa", &to, salt + u64::from(old)),
                upgrade::ready_marker("aaa", &to, held.salt + u64::from(new)),
                "no READY from before the word answers a notice after it"
            );
        }
    }
    assert!(
        (asked_from..=asked_by).contains(&held.request_at),
        "when the word was given"
    );
    ask(&o, TAB, Ask::Now, false).expect("--now");
    let hurried = load(&o, "aaa").expect("state");
    assert!(hurried.salt > held.salt, "the owner's word opens a round");
    let kept = St::for_target(
        Some(hurried.clone()),
        &Version::parse("2.1.281").expect("v"),
        &upgrade::Candidate {
            exe: PathBuf::from("/pkg/claude"),
            version: to.clone(),
            source: upgrade::Source::Managed,
        },
        None,
        super::super::Proc::default(),
        now_s(),
    );
    assert_eq!(kept.salt, hurried.salt);
}

#[test]
fn an_owners_now_quiets_an_overdue_stall_only_for_a_bounded_time() {
    let shell = Row {
        wait: "not-idle:shell".to_string(),
        request: Request::Now,
        request_at: NOW,
        ..pending(7 * 3_600)
    };
    let soon = NOW + NOW_QUIETS_S - 1;
    assert_eq!(shell.stall(soon), None, "the owner has just acted");
    assert_eq!(shell.remedy(soon), None);
    assert!(shell.moving(soon));
    assert_eq!(shell.badge(soon), None, "the mark is lowered");
    let stopped = Row {
        phase: Phase::Failed("argv:--print".into()),
        ..shell.clone()
    };
    assert!(stopped.stall(soon).is_some(), "a stop is still a stall");
    let later = NOW + NOW_QUIETS_S;
    assert_eq!(shell.stall(later).as_deref(), Some("overdue"));
    assert_eq!(shell.remedy(later), Some(Remedy::Waits));
    assert_eq!(shell.badge(later), None, "never a mark: the column says it");
    let month = NOW + 30 * 86_400;
    assert_eq!(shell.stall(month).as_deref(), Some("overdue"));
    let untimed = Row {
        request_at: 0,
        ..shell
    };
    assert_eq!(untimed.stall(NOW).as_deref(), Some("overdue"));
}

#[test]
fn a_line_refusal_is_a_digest_line_mended_by_hand() {
    let row = Row {
        phase: Phase::Failed("line:the line would be 1100 bytes (max 1000)".to_string()),
        ..pending(60)
    };
    assert_eq!(row.badge(NOW), None, "a digest line, not a mark");
    assert!(
        matches!(row.attention(NOW), Attention::Digest(n) if n.kind == "refused-launch"),
        "{:?}",
        row.attention(NOW)
    );
    assert_eq!(row.remedy(NOW), Some(Remedy::ByHand));
}

#[cfg(unix)]
fn parked_agent(tab: &str) -> aterm_uds::spawnfd::Child {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("exe"))
        .args(["harness::upgrade_drive::tests::park_when_asked", "--exact"])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in agent");
    for _ in 0..3_000 {
        if atpkg::caller_shell::process_args(child.id())
            .is_some_and(|a| a.env_var("UPGRADE_DRIVE_TEST_PARK").is_some())
        {
            return child;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid = child.id();
    let _ = child.kill();
    let _ = child.wait();
    panic!("the stand-in agent {pid} never started");
}

fn register_as(home: &std::path::Path, pid: u32, session: &str, version: &str) {
    let start = super::super::kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("sessions");
    std::fs::write(
        home.join(format!(".claude/sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"{version}","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
}

#[cfg(unix)]
#[test]
fn a_state_with_no_recorded_tab_stands_in_the_tab_its_holder_is_in() {
    let dir = ScratchDir::new("upgrade-status", "untabbed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let session = "0badf00d-1111-2222-3333-444455556666";
    let older = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "native".to_string(),
        salt: now_s() - 3_600,
        ..St::default()
    };
    save(&o, session, &older);
    let older = load(&o, session).expect("saved");
    let for_tab = |tab: &str| Opts {
        only_sid: Some(tab.to_string()),
        ..o.clone()
    };
    let other = "s-4906566e7ab0a0c15ee3";

    let mut elsewhere = parked_agent(other);
    register_as(&o.home, elsewhere.id(), session, "2.1.281");
    let (listed_elsewhere, _) = status_rows(&for_tab(TAB), true);
    let asked_elsewhere = ask(&o, TAB, Ask::Now, false);
    let _ = elsewhere.kill();
    let _ = elsewhere.wait();
    let untouched = load(&o, session);

    let mut here = parked_agent(TAB);
    register_as(&o.home, here.id(), session, "2.1.281");
    let (listed, vetted) = status_rows(&for_tab(TAB), true);
    let asked = ask(&o, TAB, Ask::Now, false);
    let _ = here.kill();
    let _ = here.wait();
    let written = load(&o, session).expect("state");

    assert!(listed_elsewhere.is_empty(), "{listed_elsewhere:?}");
    let e = asked_elsewhere.expect_err("no holder in this tab");
    assert!(e.contains("no upgrade is recorded"), "{e}");
    assert_eq!(untouched, Some(older), "nothing written");
    assert!(vetted);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].tab, TAB, "shown under the holder's tab");
    assert_eq!(asked.expect("the word lands").request, Request::Now);
    assert_eq!(written.tab, TAB, "the tab is recorded with the word");
    assert_eq!(written.request_tab, TAB);
}

fn register_status(
    home: &std::path::Path,
    pid: u32,
    session: &str,
    version: &str,
    status: &str,
    since_s: u64,
) {
    let start = super::super::kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("sessions");
    std::fs::write(
        home.join(format!(".claude/sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"{version}","status":"{status}","statusUpdatedAt":{},"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#,
            since_s * 1000
        ),
    )
    .expect("session file");
}

#[cfg(unix)]
#[test]
fn an_unreached_row_reads_claudes_live_status_never_over_a_recorded_word() {
    let dir = ScratchDir::new("upgrade-status", "unreached");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let mut agent = parked_agent(TAB);
    let session = "0badf00d-1111-2222-3333-444455556666";
    let group = super::super::process_group(agent.id()).expect("group");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(group),
    }];
    let now = now_s();
    let status_since = now - 2 * 3_600;
    let cases = [
        (Phase::Pending, "", "waiting"),
        (Phase::Pending, "", "shell"),
        (Phase::Pending, "", "idle"),
    ];
    let mut seen = Vec::new();
    for (phase, recorded, status) in &cases {
        save(
            &o,
            session,
            &St {
                phase: phase.clone(),
                from: "2.1.281".to_string(),
                to: "2.1.282".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: now - 7 * 3_600,
                wait: (*recorded).to_string(),
                wait_since: if recorded.is_empty() { 0 } else { now - 600 },
                ..St::default()
            },
        );
        register_status(
            &o.home,
            agent.id(),
            session,
            "2.1.281",
            status,
            status_since,
        );
        let (cli, vetted) = status_rows(&o, true);
        let mut view = View::default();
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let _ = view.refresh_with(
            &o,
            &tabs,
            now,
            &|sessions| holders(&o, sessions, Some(&tabs)),
            &mut |rows| sent.push(rows.to_vec()),
        );
        seen.push((vetted, cli, sent));
    }
    let _ = agent.kill();
    let _ = agent.wait();

    let want = [
        ("not-idle:waiting", Remedy::Waits, "2h"),
        ("not-idle:shell", Remedy::Waits, "2h"),
        ("", Remedy::Waits, "-"),
    ];
    for (((phase, recorded, status), (vetted, cli, sent)), (wait, remedy, wait_for)) in
        cases.iter().zip(&seen).zip(want)
    {
        let case = format!("{} {recorded:?} under {status}", phase.word());
        assert!(vetted, "{case}");
        assert_eq!(cli.len(), 1, "{case}: {cli:?}");
        assert_eq!(sent.len(), 1, "{case}: {sent:?}");
        assert_eq!(sent[0].len(), 1, "{case}: {sent:?}");
        for (surface, row) in [("--status", &cli[0]), ("window", &sent[0][0])] {
            assert_eq!(row.wait, wait, "{case} ({surface})");
            assert_eq!(row.remedy(now), Some(remedy), "{case} ({surface})");
            let line = row.line(now);
            let dash = if wait.is_empty() { "-" } else { wait };
            assert!(
                line.contains(&format!(" wait={dash} wait_for={wait_for} ")),
                "{case} ({surface}): {line}"
            );
        }
    }
    assert_eq!(
        seen[0].1[0].column(now),
        "stalled/2.1.282/overdue:not-idle:waiting/7h"
    );
}

#[test]
fn a_row_read_off_a_live_status_names_its_tab_for_the_host_to_follow() {
    let dir = ScratchDir::new("upgrade-status", "follows");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let session = "0badf00d-1111-2222-3333-444455556666";
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: None,
    }];
    let now = now_s();
    let record = |wait: &str| {
        save(
            &o,
            session,
            &St {
                phase: Phase::Pending,
                from: "2.1.281".to_string(),
                to: "2.1.282".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: now - 7 * 3_600,
                wait: wait.to_string(),
                wait_since: if wait.is_empty() { 0 } else { now - 600 },
                ..St::default()
            },
        );
    };
    let held = |status: &'static str, since: u64| {
        move |sessions: &BTreeSet<String>| {
            Some(
                sessions
                    .iter()
                    .map(|s| Holder {
                        status: status.to_string(),
                        status_since: since,
                        ..holder(s, "2.1.281", Some(TAB))
                    })
                    .collect(),
            )
        }
    };
    let mut view = View::default();
    let mut sent: Vec<Vec<Row>> = Vec::new();

    record("");
    let next = view.refresh_with(&o, &tabs, now, &held("waiting", now - 600), &mut |rows| {
        sent.push(rows.to_vec());
    });
    let boxed = (next, view.follows().to_vec());
    let watch = super::super::watch_at(&load(&o, session).expect("the record"));
    let _ = view.refresh_with(&o, &tabs, now + 5, &held("busy", now + 1), &mut |rows| {
        sent.push(rows.to_vec());
    });
    let turn = view.follows().to_vec();
    record("settling");
    let _ = view.refresh_with(
        &o,
        &tabs,
        now + 10,
        &held("waiting", now + 8),
        &mut |rows| {
            sent.push(rows.to_vec());
        },
    );
    let recorded = view.follows().to_vec();

    assert_eq!(
        boxed.0, watch,
        "no instant names a status's move: only the record's watch"
    );
    assert_eq!(boxed.1, [TAB], "the tab is followed instead");
    assert_eq!(sent.len(), 3, "{sent:?}");
    let (b, t) = (&sent[0][0], &sent[1][0]);
    assert_eq!(
        (b.wait.as_str(), b.remedy(now)),
        ("not-idle:waiting", Some(Remedy::Waits))
    );
    assert_eq!(
        (t.wait.as_str(), t.remedy(now + 5)),
        ("not-idle:busy", Some(Remedy::Waits)),
        "a turn still running is no Upgrade now (ruling 380)"
    );
    assert_eq!(turn, [TAB], "still followed while its row reads the status");
    assert_eq!(sent[2][0].wait, "settling");
    assert!(
        recorded.is_empty(),
        "a recorded word is followed by nothing"
    );
}

#[test]
fn each_kind_of_stall_names_the_remedy_that_moves_it() {
    let base = pending(60);
    assert_eq!(base.remedy(NOW), None, "not stalled: nothing to do");
    for (wait, remedy) in [
        ("", Remedy::Waits),
        ("settling", Remedy::Waits),
        ("attended", Remedy::Waits),
        ("busy", Remedy::Waits),
        ("not-idle:busy", Remedy::Waits),
        ("left-typed-backoff", Remedy::Now),
        ("queued", Remedy::Now),
        ("not-idle:shell", Remedy::Waits),
        ("status-stale:busy", Remedy::Waits),
        ("not-idle:waiting", Remedy::Waits),
        ("not-idle", Remedy::Waits),
        ("awaiting-ready", Remedy::Waits),
        ("draft", Remedy::Waits),
        ("box", Remedy::Waits),
        ("held", Remedy::Waits),
        ("background", Remedy::Waits),
    ] {
        assert_eq!(
            pending_with(wait).remedy(NOW),
            Some(remedy),
            "overdue, waiting {wait:?}"
        );
    }
    for (phase, wait, remedy) in [
        (Phase::Failed("unanswered".into()), "", Remedy::AskAgain),
        (Phase::Pending, "terminal:tmux", Remedy::InItsPane),
        (Phase::Failed("not-a-shell-job".into()), "", Remedy::ByHand),
        (Phase::Failed("argv:--print".into()), "", Remedy::ByHand),
        (Phase::Failed("no-resume".into()), "", Remedy::ByHand),
    ] {
        let r = Row {
            phase: phase.clone(),
            wait: wait.to_string(),
            ..base.clone()
        };
        assert_eq!(r.remedy(NOW), Some(remedy), "{phase:?} {wait}");
    }
}

#[test]
fn a_codex_upgrade_is_listed_held_and_hurried_like_claudes() {
    let dir = ScratchDir::new("upgrade-status", "codex");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let session = format!("codex-{TAB}");
    save(
        &o,
        &session,
        &St {
            agent: upgrade::Agent::Codex,
            phase: Phase::Pending,
            from: "0.157.0".to_string(),
            to: "0.157.1".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 120,
            wait: "daemon-first".to_string(),
            wait_since: now_s() - 60,
            ..St::default()
        },
    );
    let all = rows(&o);
    assert_eq!(all.len(), 1);
    let row = &all[0];
    assert_eq!(row.agent, upgrade::Agent::Codex);
    assert_eq!(row.session, session);
    assert!(
        row.column(now_s())
            .starts_with("pending/0.157.1/daemon-first/")
    );
    assert_eq!(
        row.to_json(now_s()).get("agent").and_then(Value::as_str),
        Some("Codex")
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let behind = |s: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(s.iter().map(|s| holder(s, "0.157.0", Some(TAB))).collect())
    };
    let current = |s: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(s.iter().map(|s| holder(s, "0.157.1", Some(TAB))).collect())
    };
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &behind, &mut |r| sent.push(r.to_vec()));
    assert_eq!(sent.last().map(Vec::len), Some(1), "listed while behind");
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &current, &mut |r| sent.push(r.to_vec()));
    assert_eq!(
        sent.last().map(Vec::len),
        Some(0),
        "gone once its TUI is current"
    );
    let held = ask(&o, TAB, Ask::Defer(3_600), false).expect("a Codex upgrade to defer");
    assert_eq!(held.agent, upgrade::Agent::Codex);
    assert!(matches!(held.request, Request::DeferUntil(_)));
    assert!(held.column(now_s()).starts_with("deferred/0.157.1/"));
    let skipped = ask(&o, TAB, Ask::Skip, false).expect("a Codex upgrade to skip");
    assert_eq!(skipped.request, Request::Skip("0.157.1".to_string()));
    let hurried = ask(&o, TAB, Ask::Now, false).expect("a Codex upgrade to hurry");
    assert_eq!(hurried.request, Request::Now);
    let st = load(&o, &session).expect("the state");
    assert_eq!(st.agent, upgrade::Agent::Codex, "the word keeps the lane");
    assert_eq!(st.request_tab, TAB);
}

fn moved_state(o: &Opts, agent: upgrade::Agent, phase: Phase, exited_at: u64) -> String {
    moved_state_as(o, agent, phase, exited_at, "")
}

fn moved_state_as(
    o: &Opts,
    agent: upgrade::Agent,
    phase: Phase,
    exited_at: u64,
    cause: &str,
) -> String {
    let (session, from, to) = match agent {
        upgrade::Agent::Claude => (
            "0badf00d-1111-2222-3333-444455556666".to_string(),
            "2.1.281",
            "2.1.283",
        ),
        upgrade::Agent::Codex => (format!("codex-{TAB}"), "0.157.0", "0.157.1"),
        upgrade::Agent::Muse => unreachable!("Muse Code's quiet upgrade keeps no ladder state"),
        upgrade::Agent::Agy => unreachable!("agy's quiet upgrade keeps no ladder state"),
    };
    save(
        o,
        &session,
        &St {
            agent,
            phase,
            from: from.to_string(),
            to: to.to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 600,
            exited_at,
            cause: cause.to_string(),
            ..St::default()
        },
    );
    session
}

fn fresh_claude_state(o: &Opts, exited_at: u64) -> String {
    moved_state_as(
        o,
        upgrade::Agent::Claude,
        Phase::Failed("no-resume".to_string()),
        exited_at,
        crate::harness::relaunch::CAUSE_UPGRADE_FRESH,
    )
}

#[test]
fn a_move_that_failed_after_its_exit_stands_with_no_holder() {
    use upgrade::Agent::{Claude, Codex};
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    for (agent, why) in [
        (Claude, "stale-exit"),
        (Claude, "shell-gone"),
        (Claude, "no-resume"),
        (Claude, "relaunch-refused"),
        (Codex, "no-resume-hint"),
        (Codex, "hint-mismatch"),
        (Codex, "stale-exit"),
    ] {
        let dir = ScratchDir::new("upgrade-status", &format!("failed-{agent:?}-{why}"));
        let o = opts(&dir);
        let exited = now_s() - 120;
        moved_state(&o, agent, Phase::Failed(why.to_string()), exited);
        let (shown, vetted) = status_rows(&o, true);
        assert!(vetted, "{agent:?} {why}");
        assert_eq!(shown.len(), 1, "{agent:?} {why}: listed by --status");
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let mut view = View::default();
        view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |r| {
            sent.push(r.to_vec());
        });
        let rows = sent.last().cloned().unwrap_or_default();
        assert_eq!(rows.len(), 1, "{agent:?} {why}: handed to the window");
        assert_eq!(rows[0].stall(now_s()), Some(format!("failed:{why}")));
        assert_eq!(
            rows[0].remedy(now_s()),
            Some(Remedy::ResumeInTab),
            "{agent:?} {why}"
        );
        assert!(!rows[0].asks_on_its_own(now_s()), "{agent:?} {why}");
        let _ = ask(&o, TAB, Ask::Now, false).expect_err("stopped");
        let mut view = View::default();
        view.refresh_with(
            &o,
            &tabs,
            exited + EXITED_FAILURE_SHOWN_S + 1,
            &nobody,
            &mut |r| sent.push(r.to_vec()),
        );
        assert_eq!(sent.last().map(Vec::len), Some(0), "{agent:?} {why}");
    }
    let dir = ScratchDir::new("upgrade-status", "cl-failed-before");
    let o = opts(&dir);
    moved_state(&o, Claude, Phase::Failed("signal-refused".to_string()), 0);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |r| sent.push(r.to_vec()));
    assert_eq!(sent.last().map(Vec::len), Some(0));
    let dir = ScratchDir::new("upgrade-status", "cl-failed-resumed");
    let o = opts(&dir);
    let resumed = moved_state(
        &o,
        Claude,
        Phase::Failed("stale-exit".to_string()),
        now_s() - 60,
    );
    let mut old = load(&o, &resumed).unwrap();
    old.proc_start = "Mon Oct 26 12:00:00 2026".into();
    save(&o, &resumed, &old);
    let held_on = |version: &'static str| {
        move |s: &BTreeSet<String>| -> Option<Vec<Holder>> {
            Some(
                s.iter()
                    .map(|s| Holder {
                        proc_start: "Mon Oct 26 12:01:00 2026".into(),
                        ..holder(s, version, Some(TAB))
                    })
                    .collect(),
            )
        }
    };
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &held_on("2.1.281"), &mut |r| {
        sent.push(r.to_vec());
    });
    let rows = sent.last().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "held again on the old build: shown");
    assert_eq!(rows[0].remedy(now_s()), Some(Remedy::ByHand));
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &held_on("2.1.283"), &mut |r| {
        sent.push(r.to_vec());
    });
    assert_eq!(sent.last().map(Vec::len), Some(0), "on the target: gone");
}

const NEW: &str = "5eed0000-1111-2222-3333-444455556666";

#[cfg(unix)]
#[test]
fn a_claude_move_down_after_its_exit_is_lowered_by_any_agent_back_in_its_tab() {
    let dir = ScratchDir::new("upgrade-status", "agent-back");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let exited = now_s() - 120;
    let down = |exited_at: u64| {
        moved_state_as(
            &o,
            upgrade::Agent::Claude,
            Phase::Failed("no-resume".to_string()),
            exited_at,
            fresh,
        )
    };
    let old = down(exited);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let in_tab = |tab: Option<&'static str>, led: bool| {
        move |_: &BTreeSet<String>| -> Option<Vec<Holder>> {
            Some(vec![Holder {
                led,
                ..holder(NEW, "2.1.283", tab)
            }])
        }
    };
    let meta = || -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
            .unwrap_or_default()
    };
    let clear = || asked.lock().expect("log").clear();
    let unclaimed = Aside::default;
    let listed = |held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        status_rows_with(&o, now_s(), held, &unclaimed).0.len()
    };
    let look = |w: &mut (View, Vec<Row>),
                held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>|
     -> usize {
        let (view, shown) = w;
        let _ = view.refresh_with(&o, &tabs, now_s(), held, &mut |r| *shown = r.to_vec());
        shown.len()
    };

    let mut w = (View::default(), Vec::new());
    assert_eq!(look(&mut w, &nobody), 1);
    assert_eq!(w.1[0].session, old);
    match w.1[0].attention(now_s()) {
        Attention::Interrupt(need) => {
            assert_eq!(need.kind, "agent-down");
            assert_eq!(need.command.as_deref(), Some("claude"));
        }
        other => panic!("the owner is needed: {other:?}"),
    }
    let badge = w.1[0].badge(now_s()).expect("an interrupt is marked");
    let set = format!("@{TAB} meta set attention owner={ATTENTION_OWNER} {badge}");
    assert!(meta().contains(&set), "{:?}", meta());
    assert_eq!(w.0.follows(), [TAB.to_string()], "its screen is followed");
    assert_eq!(listed(&nobody), 1);

    for held in [in_tab(Some("s-other"), true), in_tab(None, true)] {
        assert_eq!(look(&mut w, &held), 1);
        assert_eq!(listed(&held), 1);
    }
    assert_eq!(look(&mut w, &in_tab(Some(TAB), false)), 1);
    assert_eq!(listed(&in_tab(Some(TAB), false)), 0);
    assert_eq!(w.0.follows(), [TAB.to_string()]);

    clear();
    assert_eq!(look(&mut w, &in_tab(Some(TAB), true)), 0);
    assert_eq!(
        meta(),
        [format!(
            "@{TAB} meta unset attention owner={ATTENTION_OWNER}"
        )]
    );
    assert!(w.0.follows().is_empty(), "{:?}", w.0.follows());
    assert_eq!(listed(&in_tab(Some(TAB), true)), 0);
    clear();
    assert_eq!(look(&mut w, &nobody), 0);
    assert_eq!(look(&mut w, &nobody), 0);
    assert!(
        !meta().iter().any(|l| l.contains(" meta set ")),
        "{:?}",
        meta()
    );
    let mut afresh = (View::default(), Vec::new());
    assert_eq!(look(&mut afresh, &nobody), 0);
    assert_eq!(listed(&nobody), 1);
    std::fs::remove_file(state_dir(&o).join(BACK_FILE)).expect("kept beside the records");
    let mut unkept = (View::default(), Vec::new());
    assert_eq!(look(&mut unkept, &nobody), 1);

    down(exited + 30);
    assert_eq!(look(&mut w, &nobody), 1);
    assert_eq!(look(&mut w, &in_tab(Some(TAB), true)), 0);
    moved_state_as(
        &o,
        upgrade::Agent::Claude,
        Phase::Relaunched { at_s: now_s() },
        exited + 30,
        fresh,
    );
    assert_eq!(look(&mut w, &nobody), 1, "a restart in flight is shown");
    down(exited + 30);
    assert_eq!(look(&mut w, &nobody), 1);
    assert!(matches!(
        w.1[0].attention(now_s()),
        Attention::Interrupt(ref need) if need.kind == "agent-down"
    ));
}

#[cfg(unix)]
#[test]
fn a_fresh_claude_in_the_down_tab_is_read_by_the_real_holders() {
    const NEW2: &str = "5eed0000-2222-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "agent-back-real");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let exited = now_s() - 120;
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let down = |exited_at: u64| {
        moved_state_as(
            &o,
            upgrade::Agent::Claude,
            Phase::Failed("stale-exit".to_string()),
            exited_at,
            fresh,
        )
    };
    let old = down(exited);
    let vet: BTreeSet<String> = [old].into();
    let mut agent = parked_agent(TAB);
    register_as(&o.home, agent.id(), NEW, "2.1.283");
    let group = super::super::process_group(agent.id()).expect("group");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(group),
    }];
    let of = |held: Option<Vec<Holder>>, session: &str| -> Vec<Holder> {
        held.expect("read whole")
            .into_iter()
            .filter(|h| h.session == session)
            .collect()
    };
    let window = of(holders(&o, &vet, Some(&tabs)), NEW);
    assert_eq!(window.len(), 1, "{window:?}");
    assert_eq!(window[0].tab.as_deref(), Some(TAB));
    assert!(window[0].led, "proven by its group: the tab's agent");
    let cli = of(holders(&o, &vet, None), NEW);
    assert_eq!(cli.len(), 1, "{cli:?}");
    assert_eq!(cli[0].tab.as_deref(), Some(TAB));
    assert!(!cli[0].led, "by its environment alone");

    let mut view = View::default();
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|s| holders(&o, s, Some(&tabs)),
        &mut |r| sent.push(r.to_vec()),
    );
    assert_eq!(sent, [Vec::<Row>::new()]);
    assert!(status_rows(&o, false).0.is_empty());

    std::fs::remove_file(state_dir(&o).join(BACK_FILE)).expect("kept beside the records");
    let by_env = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(1),
    }];
    let held = of(holders(&o, &vet, Some(&by_env)), NEW);
    assert!(
        held.len() == 1 && held[0].tab.as_deref() == Some(TAB) && !held[0].led,
        "{held:?}"
    );
    let mut kept: Vec<Row> = Vec::new();
    let _ = View::default().refresh_with(
        &o,
        &by_env,
        now_s(),
        &|s| holders(&o, s, Some(&by_env)),
        &mut |r| kept = r.to_vec(),
    );
    assert_eq!(kept.len(), 1);

    let mut second = parked_agent("s-other");
    register_as(&o.home, second.id(), NEW2, "2.1.283");
    assert!(of(holders(&o, &vet, Some(&tabs)), NEW2).is_empty());
    assert!(of(holders(&o, &vet, None), NEW2).is_empty());
    let _ = second.kill();
    let _ = second.wait();

    let file = o.home.join(format!(".claude/sessions/{}.json", agent.id()));
    let interactive = std::fs::read_to_string(&file).expect("its file");
    std::fs::write(&file, interactive.replace("\"cli\"", "\"sdk-cli\"")).expect("rewrite");
    assert!(of(holders(&o, &vet, Some(&tabs)), NEW).is_empty());
    std::fs::write(&file, &interactive).expect("restore");
    assert_eq!(of(holders(&o, &vet, Some(&tabs)), NEW).len(), 1);
    moved_state(
        &o,
        upgrade::Agent::Claude,
        Phase::Failed("signal-refused".to_string()),
        0,
    );
    assert!(of(holders(&o, &vet, Some(&tabs)), NEW).is_empty());
    down(now_s() - EXITED_FAILURE_SHOWN_S - 1);
    assert!(of(holders(&o, &vet, Some(&tabs)), NEW).is_empty());
    down(exited);

    let _ = agent.kill();
    let _ = agent.wait();
    assert!(of(holders(&o, &vet, Some(&tabs)), NEW).is_empty());
    assert_eq!(status_rows(&o, false).0.len(), 1);
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|s| holders(&o, s, Some(&tabs)),
        &mut |r| sent.push(r.to_vec()),
    );
    assert_eq!(sent, [Vec::<Row>::new()], "never handed over again");
}

#[test]
fn a_shell_gone_row_is_answered_by_one_claude_started_after_it_in_its_project() {
    let user = PathBuf::from("/u");
    let mine = user.join(".claude");
    let row = |session: &str, cwd: &str, exited_at: u64| ShellGone {
        session: session.to_string(),
        dir: Some(mine.clone()),
        cwd: cwd.to_string(),
        exited_at,
    };
    let agent = |pid: u32, cwd: &str, dir: Option<&Path>| {
        let mut sf = upgrade::parse_session_file(&format!(
            r#"{{"pid":{pid},"sessionId":"5eed0000-1111-2222-3333-44445555{pid:04}","cwd":"{cwd}","version":"2.1.283","status":"idle","statusUpdatedAt":1,"procStart":"Sat Oct 17 10:00:00 2026","kind":"interactive","entrypoint":"cli"}}"#
        ))
        .expect("a session file");
        sf.claude_dir = dir.map(Path::to_path_buf);
        let h = Holder {
            led: true,
            ..holder(&sf.session_id, "2.1.283", Some("s-new"))
        };
        (sf, h)
    };
    let started = |pid: u32| -> Option<u64> {
        match pid {
            1 => Some(1_050),
            2 => Some(1_200),
            3 => Some(1_000),
            _ => None,
        }
    };
    let answered = |gone: &[ShellGone], others: &[(upgrade::SessionFile, Holder)]| {
        answering_shell_gone(
            &user,
            gone,
            others.iter().map(|(sf, h)| (sf, h.clone())).collect(),
            started,
        )
        .into_iter()
        .map(|h| (h.session, h.answers.unwrap_or_default()))
        .collect::<Vec<_>>()
    };
    let sid = |pid: u32| format!("5eed0000-1111-2222-3333-44445555{pid:04}");

    let a = [row("row-a", "/p", 1_000)];
    for dir in [Some(mine.as_path()), None] {
        assert_eq!(
            answered(&a, &[agent(1, "/p", dir)]),
            [(sid(1), "row-a".to_string())]
        );
    }
    assert!(answered(&a, &[agent(3, "/p", Some(&mine))]).is_empty());
    assert!(answered(&a, &[agent(1, "/q", Some(&mine))]).is_empty());
    assert!(answered(&a, &[agent(1, "/p", Some(Path::new("/u/.claude-work")))]).is_empty());
    assert!(answered(&a, &[agent(9, "/p", Some(&mine))]).is_empty());
    assert!(answered(&[row("row-a", "", 1_000)], &[agent(1, "", Some(&mine))]).is_empty());

    let two = [row("row-b", "/p", 1_100), row("row-a", "/p", 1_000)];
    let both = [agent(2, "/p", Some(&mine)), agent(1, "/p", Some(&mine))];
    assert_eq!(
        answered(&two, &both),
        [(sid(1), "row-a".to_string()), (sid(2), "row-b".to_string())]
    );
    assert_eq!(
        answered(&two, &[agent(2, "/p", Some(&mine))]),
        [(sid(2), "row-a".to_string())]
    );
    assert_eq!(
        answered(&two, &[agent(1, "/p", Some(&mine))]),
        [(sid(1), "row-a".to_string())]
    );
}

#[cfg(unix)]
#[test]
fn a_claude_started_in_a_new_tab_answers_the_row_whose_shell_is_gone() {
    let dir = ScratchDir::new("upgrade-status", "shell-gone-answered");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let exited = now_s() - 120;
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let down = |why: &str, cwd: &str| {
        let session = moved_state_as(
            &o,
            upgrade::Agent::Claude,
            Phase::Failed(why.to_string()),
            exited,
            fresh,
        );
        let mut st = load(&o, &session).expect("the state");
        cwd.clone_into(&mut st.cwd);
        save(&o, &session, &st);
        session
    };
    let old = down("shell-gone", "/elsewhere");
    let vet: BTreeSet<String> = [old.clone()].into();
    let mut agent = parked_agent("s-new");
    register_as(&o.home, agent.id(), NEW, "2.1.283");
    let group = super::super::process_group(agent.id()).expect("group");
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: None,
        },
        LiveTab {
            sid: "s-new".to_string(),
            fgpgid: Some(group),
        },
    ];
    let answers = |held: Option<Vec<Holder>>| -> Vec<(Option<String>, bool)> {
        held.expect("read whole")
            .into_iter()
            .filter(|h| h.session == NEW)
            .map(|h| (h.answers, h.led))
            .collect()
    };
    let shown = || -> usize {
        let mut rows: Vec<Row> = Vec::new();
        let _ = View::default().refresh_with(
            &o,
            &tabs,
            now_s(),
            &|s| holders(&o, s, Some(&tabs)),
            &mut |r| rows = r.to_vec(),
        );
        rows.len()
    };

    assert!(answers(holders(&o, &vet, Some(&tabs))).is_empty());
    assert_eq!(shown(), 1);
    assert_eq!(status_rows(&o, false).0.len(), 1);

    down("shell-gone", "/");
    assert_eq!(
        answers(holders(&o, &vet, Some(&tabs))),
        [(Some(old.clone()), true)]
    );
    assert_eq!(
        answers(holders(&o, &vet, None)),
        [(Some(old.clone()), false)]
    );
    assert!(status_rows(&o, false).0.is_empty());
    assert_eq!(shown(), 0);
    std::fs::remove_file(state_dir(&o).join(BACK_FILE)).expect("kept beside the records");

    down("no-resume", "/");
    assert!(answers(holders(&o, &vet, Some(&tabs))).is_empty());
    assert_eq!(shown(), 1);
    assert_eq!(status_rows(&o, false).0.len(), 1);

    down("shell-gone", "/");
    let _ = agent.kill();
    let _ = agent.wait();
    assert!(answers(holders(&o, &vet, Some(&tabs))).is_empty());
    assert_eq!(status_rows(&o, false).0.len(), 1);
}

#[test]
fn a_rearmed_round_forgets_when_the_old_restarts_agent_was_seen_gone() {
    let mut st = St {
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: NOW - 3 * upgrade::RETRY_S,
        failed_at: NOW - 2 * upgrade::RETRY_S,
        resumed_pid: 4_000_002,
        relaunched_pid: 4_000_002,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    assert!(
        Row::of("c", &st, NOW).failed_after_exit(),
        "the old restart's"
    );
    let _ = st.rearm(NOW);
    assert_eq!((st.exited_at, st.resumed_pid, st.relaunched_pid), (0, 0, 0));
    st.fail("resumed-elsewhere", NOW + 60);
    assert!(!Row::of("c", &st, NOW + 60).failed_after_exit(), "{st:?}");
}

#[test]
fn a_restart_under_way_past_its_bound_reads_stuck_and_says_what_it_waits_on() {
    let stale = super::super::STALE_S;
    let row = |phase: Phase, exited_at: u64| Row {
        phase,
        exited_at,
        ..pending(120)
    };
    let young = row(Phase::Exiting { at_s: NOW - stale }, 0);
    assert_eq!(young.stall(NOW), None, "within the bound");
    assert_eq!(young.column(NOW), "restarting/2.1.282/exiting/2m");
    assert!(young.moving(NOW));
    for (phase, exited_at, kind) in [
        (
            Phase::Exiting {
                at_s: NOW - stale - 1,
            },
            0,
            "stuck:exiting",
        ),
        (
            Phase::Exiting {
                at_s: NOW - stale - 1,
            },
            NOW - stale,
            "stuck:exited",
        ),
        (
            Phase::Relaunched {
                at_s: NOW - stale - 1,
            },
            NOW - stale - 30,
            "stuck:relaunched",
        ),
    ] {
        let r = row(phase.clone(), exited_at);
        assert_eq!(r.stall(NOW).as_deref(), Some(kind), "{phase:?}");
        assert_eq!(r.column(NOW), format!("stalled/2.1.282/{kind}/2m"));
        assert_eq!(r.remedy(NOW), Some(Remedy::Waits), "{kind}");
        assert!(!r.moving(NOW), "{kind}");
        assert_eq!(r.badge(NOW), None, "{kind}: too soon to mark a tab");
        let late = NOW + 3_600;
        assert!(r.badge(late).is_some_and(|b| b.len() <= 200), "{kind}");
        assert_eq!(r.badge(late), r.badge(late + 3_600), "{kind}: stable");
    }
}

#[test]
fn the_owner_is_told_which_processes_hold_the_move() {
    let started = NOW - (5 * 86_400 + 4 * 3_600);
    let shell = |pid: u32| HeldBy {
        pid,
        name: "zsh".to_string(),
        since: started,
    };
    let row = Row {
        wait: "background".to_string(),
        held_by: vec![shell(63_492), shell(63_493)],
        ..pending(5 * 86_400 + 12 * 3_600)
    };
    let line = row.line(NOW);
    assert!(
        line.contains(" stalled=overdue held_by=63492(zsh:5d4h),63493(zsh:5d4h) release=- "),
        "{line}"
    );
    let json = row.to_json(NOW);
    let held = json
        .get("held_by")
        .and_then(Value::as_array)
        .expect("a list");
    assert_eq!(held.len(), 2);
    assert_eq!(held[0].get("pid").and_then(Value::as_u64), Some(63_492));
    assert_eq!(held[0].get("name").and_then(Value::as_str), Some("zsh"));
    assert_eq!(
        held[0].get("age_s").and_then(Value::as_u64),
        Some(5 * 86_400 + 4 * 3_600)
    );
    let many = Row {
        held_by: (1..=7).map(shell).collect(),
        ..row.clone()
    };
    assert!(
        many.line(NOW).contains(",5(zsh:5d4h),+2 release=- "),
        "{}",
        many.line(NOW)
    );
    let free = pending(7 * 3_600);
    assert!(
        free.line(NOW).contains(" held_by=- release=- "),
        "{}",
        free.line(NOW)
    );
    assert_eq!(free.held_words(NOW), None);
    assert!(
        free.to_json(NOW)
            .get("held_by")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    );
}

#[test]
fn a_release_still_owed_is_shown() {
    let owed = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 8 * 3_600,
        failed_at: NOW - 600,
        release: "gave-up".to_string(),
        wait: "release:draft".to_string(),
        wait_since: NOW - 300,
        ..St::default()
    };
    let release = |row: &Row| {
        row.to_json(NOW)
            .get("release")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let row = Row::of("aaa", &owed, NOW);
    assert_eq!(release(&row).as_deref(), Some("gave-up"));
    let held = Row {
        wait: "release:draft".to_string(),
        release: "retargeted".to_string(),
        ..pending(10 * 86_400 + 23 * 3_600)
    };
    assert_eq!(held.remedy(NOW), Some(Remedy::Waits), "--now types no line");
    let released = Row::of(
        "aaa",
        &St {
            release: String::new(),
            wait: "failed".to_string(),
            ..owed.clone()
        },
        NOW,
    );
    assert_eq!(release(&released).as_deref(), Some(""));
    let codex = Row::of(
        "codex-s-b5cf2faabac5ce5127bd",
        &St {
            agent: upgrade::Agent::Codex,
            ..owed.clone()
        },
        NOW,
    );
    assert_eq!(release(&codex).as_deref(), Some(""));
}

#[test]
fn a_claude_stall_never_prints_its_wait_word() {
    for wait in [
        "",
        "busy",
        "not-idle:busy",
        "not-idle:waiting",
        "not-idle",
        "not-idle:shell",
        "status-stale:busy",
        "status-stale:shell",
        "background",
        "settling",
        "attended",
        "awaiting-ready",
        "not-ready",
        "draft",
        "box",
        "held",
        "limited",
        "login",
        "in-flight",
        "release:draft",
        "release:not-idle",
        upgrade::NEXT_POINT,
        NOT_FOREGROUND,
        CLAIMED,
        AUTOPILOT_OFF,
        AGENT_OFF,
        "something-new",
    ] {
        let row = Row {
            wait: wait.to_string(),
            ..pending(10 * 86_400 + 23 * 3_600)
        };
        let words = row.stall_words(NOW).expect("overdue");
        assert!(!words.contains('('), "{wait}: {words}");
        assert!(
            !wait.contains([':', '-']) || !words.contains(wait),
            "{wait}: {words}"
        );
        assert!(words.chars().count() <= 64, "{wait}: {words}");
    }
    let asked = |wait: &str| Row {
        phase: Phase::Announced { at_s: NOW, asks: 1 },
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    };
    assert!(asked("not-idle:busy").asks_on_its_own(NOW));
    assert_eq!(asked("not-idle:busy").badge(NOW), None);
    assert!(!pending_with("not-idle:busy").asks_on_its_own(NOW));
    assert!(pending_with("not-idle:busy").badge(NOW).is_none());
}

fn pending_with(wait: &str) -> Row {
    Row {
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    }
}

#[test]
fn an_overdue_move_names_its_cause_by_class() {
    let zsh = HeldBy {
        pid: 51_435,
        name: "zsh".to_string(),
        since: NOW - 3 * 86_400,
    };
    let at = |wait: &str, held: bool| Row {
        wait_seen: NOW,
        held_by: if held { vec![zsh.clone()] } else { Vec::new() },
        ..pending_with(wait)
    };
    let classes: [(&[&str], Cause, &str, bool); 9] = [
        (
            &[
                "not-idle:busy",
                "not-idle:shell",
                "busy",
                "background",
                "background-terminal",
                "status-stale:busy",
                "not-ready",
                "settling",
                "release:background",
                "busy-thread",
                "failed",
            ],
            Cause::OwnWork,
            "own-work",
            true,
        ),
        (
            &[
                "not-idle:waiting",
                "box",
                "draft",
                "login",
                "held",
                "release:draft",
                "announce-refused:draft",
                "announce-refused:held",
                "announce-refused:box",
            ],
            Cause::Person,
            "person",
            false,
        ),
        (&["attended"], Cause::Person, "person", true),
        (
            &["limited", "release:limited"],
            Cause::Limit,
            "limit",
            false,
        ),
        (
            &["announce-refused:changed", "announce-refused:yield"],
            Cause::CouldNotType,
            "notice",
            true,
        ),
        (&[""], Cause::NoLook, "unrecorded", true),
        (
            &["no-shell-integration", "screen-unreadable"],
            Cause::TabUnread,
            "tab-unread",
            false,
        ),
        (&["in-flight"], Cause::UnderWay, "under-way", false),
        (
            &[
                "tab-not-live",
                "no-socket",
                "conversation-in-other-tab",
                "notice-owned-by-other-process",
                "thread-locked",
            ],
            Cause::Unreached,
            "unreached",
            false,
        ),
    ];
    let own = at("background", true).overdue_cause(NOW);
    for (waits, class, word, passing) in classes {
        for wait in waits {
            let row = at(wait, false);
            assert_eq!(row.cause(), class, "{wait}");
            assert_eq!(row.cause_word(NOW), word, "{wait}");
            if class != Cause::Limit {
                assert_eq!(row.badge(NOW), None, "{wait}: never marked");
            }
            let under = at(wait, true);
            if passing {
                assert_eq!(under.cause(), Cause::OwnWork, "{wait} under work");
                assert_eq!(under.cause_word(NOW), "own-work", "{wait} under work");
                assert_eq!(under.overdue_cause(NOW), own, "{wait}: one text");
                assert_eq!(under.badge(NOW), None, "{wait}: never marked");
            } else {
                assert_eq!(under.cause(), class, "{wait} under work");
                assert_eq!(under.cause_word(NOW), word, "{wait} under work");
            }
            if class != Cause::OwnWork {
                assert_ne!(row.overdue_cause(NOW), own, "{wait}: its own text");
            }
        }
    }
    for wait in ["something-new", "goal-brand-new"] {
        let row = at(wait, true);
        assert_eq!(row.cause(), Cause::Unclassified(wait.to_string()));
        assert_eq!(row.cause_word(NOW), "unclassified", "{wait}");
        assert!(row.overdue_cause(NOW).contains(wait), "{wait}: named");
    }
}

#[test]
fn the_status_line_says_the_rung() {
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 10,
        ..pending(60)
    };
    let under_way = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(60)
    };
    for (row, rung) in [
        (pending(60), "prefer"),
        (pending(upgrade::RUNG_SETTLED_S), "settled"),
        (pending(upgrade::RUNG_KEYS_S), "keys-only"),
        (pending(upgrade::RUNG_LAND_S), "land"),
        (hurried, "land"),
        (under_way, "-"),
    ] {
        assert_eq!(
            row.to_json(NOW).get("rung").and_then(Value::as_str),
            Some(rung),
            "{row:?}"
        );
    }
}

#[test]
fn a_word_on_a_gave_up_upgrade_forgets_its_answers() {
    let dir = ScratchDir::new("upgrade-status", "gave-up-word");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        marker: "ATERM-UPGRADE-READY-8cd7f7eb".to_string(),
        markers: vec![
            "ATERM-UPGRADE-READY-5182b3fa".to_string(),
            "ATERM-UPGRADE-READY-8cd7f7eb".to_string(),
        ],
        release: "gave-up".to_string(),
        ..St::default()
    };
    save(&o, "aaa", &gave_up);
    assert_eq!(load(&o, "aaa").expect("state").markers.len(), 2);
    for word in [Ask::Skip, Ask::Defer(600)] {
        save(&o, "aaa", &gave_up);
        ask(&o, TAB, word, false).expect("the word");
        let held = load(&o, "aaa").expect("state");
        assert!(
            held.marker.is_empty() && held.markers.is_empty(),
            "{word:?}"
        );
        assert_eq!(held.release, "gave-up", "the release still owed");
    }
}

#[test]
fn a_stopped_round_says_when_its_next_round_starts() {
    let base = St {
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 20 * 3_600,
        ..St::default()
    };
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        failed_at: NOW - 3_600,
        ..base.clone()
    };
    let row = Row::of("aaa", &gave_up, NOW);
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(row.next_round_in(NOW), Some(upgrade::RETRY_S - 3_600));
    assert_eq!(row.column(NOW), "pending/2.1.282/next-round:1h/20h");
    let json = row.to_json(NOW);
    assert_eq!(
        json.get("retry_at").and_then(Value::as_u64),
        Some(NOW - 3_600 + upgrade::RETRY_S)
    );
    assert_eq!(
        json.get("next_round_s").and_then(Value::as_u64),
        Some(upgrade::RETRY_S - 3_600)
    );
    assert_eq!(json.get("stalled").and_then(Value::as_str), Some("overdue"));
    let old = Row::of(
        "aaa",
        &St {
            failed_at: 0,
            ..gave_up.clone()
        },
        NOW,
    );
    assert_eq!(old.next_round_in(NOW), Some(0));
    assert_eq!(old.column(NOW), "pending/2.1.282/next-round:due/20h");
    let skipped = Row::of(
        "aaa",
        &St {
            request: Request::Skip("2.1.282".to_string()),
            request_tab: TAB.to_string(),
            ..gave_up.clone()
        },
        NOW,
    );
    assert_eq!(skipped.next_round_in(NOW), None);
    let heard = St {
        ready_since: NOW - 60,
        failed_at: 0,
        wait: "not-idle:busy".to_string(),
        wait_since: NOW - 60,
        ..gave_up.clone()
    };
    let late = Row::of("aaa", &heard, NOW);
    assert!(late.late_ready);
    assert_eq!(late.next_round_in(NOW), None);
    assert_eq!(late.column(NOW), "pending/2.1.282/ready/20h");
    assert!(late.asks_on_its_own(NOW), "the upgrade working, not a row");
    let codex = Row::of(
        "codex-aaa",
        &St {
            agent: upgrade::Agent::Codex,
            ..heard.clone()
        },
        NOW,
    );
    assert!(!codex.late_ready);
    assert_eq!(codex.next_round_in(NOW), Some(0));
    assert_eq!(codex.column(NOW), "pending/2.1.282/next-round:due/20h");
    assert_eq!(Row::of("aaa", &base, NOW).next_round_in(NOW), None);

    let young = St {
        pending_since: NOW - 3_600,
        ..base.clone()
    };
    let refused = St {
        phase: Phase::Failed("not-a-shell-job".to_string()),
        failed_at: NOW - upgrade::RETRY_S,
        ..young.clone()
    };
    let row = Row::of("aaa", &refused, NOW);
    assert_eq!(row.stall(NOW).as_deref(), Some("refused:not-a-shell-job"));
    assert_eq!(row.next_round_in(NOW), Some(0), "due");
    let mut st = refused.clone();
    assert_eq!(st.rearm(NOW), "not-a-shell-job");
    assert_eq!(
        Row::of("aaa", &st, NOW).stall(NOW).as_deref(),
        Some("refused:not-a-shell-job"),
        "no flap at the re-arm"
    );
    st.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), NOW, 1);
    assert_eq!(Row::of("aaa", &st, NOW).stall(NOW), None, "met no refusal");
    let mut st = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..young
    };
    let _ = st.rearm(NOW);
    assert_eq!(Row::of("aaa", &st, NOW).stall(NOW), None);
}

#[test]
fn a_stop_that_asks_again_marks_nothing_and_one_that_repeats_carries_its_stall() {
    const NOW: u64 = 1_790_311_076;
    let base = Row {
        tab: "s-a".into(),
        from: "2.1.281".into(),
        to: "2.1.282".into(),
        behind_since: NOW - 8 * 3_600,
        retry_at: NOW + 600,
        wait: "failed".into(),
        ..Row::default()
    };
    let gave_up = Row {
        phase: Phase::Failed(upgrade::GAVE_UP.into()),
        stop_streak: 3,
        streak_why: upgrade::GAVE_UP.into(),
        ..base.clone()
    };
    assert_eq!(gave_up.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(gave_up.remedy(NOW), Some(Remedy::AskAgain));
    assert!(gave_up.asks_on_its_own(NOW) && !gave_up.repeating_stop());
    assert_eq!(gave_up.badge(NOW), None);
    let skipped = Row {
        request: Request::Skip("2.1.282".into()),
        ..gave_up.clone()
    };
    assert!(!skipped.asks_on_its_own(NOW) && skipped.remedy(NOW).is_none());

    let once = Row {
        phase: Phase::Failed("signal-refused".into()),
        stop_streak: 1,
        streak_why: "signal-refused".into(),
        ..base.clone()
    };
    assert!(once.asks_on_its_own(NOW));
    assert_eq!(once.badge(NOW), None);

    let twice = Row {
        stop_streak: 2,
        ..once.clone()
    };
    assert!(twice.repeating_stop() && !twice.asks_on_its_own(NOW));
    assert_eq!(gave_up.column(NOW), "pending/2.1.282/next-round:10m/8h");
    assert_eq!(once.column(NOW), "pending/2.1.282/next-round:10m/8h");
    assert_eq!(
        twice.column(NOW),
        "stalled/2.1.282/failed:signal-refused/8h"
    );
    assert_eq!(twice.badge(NOW), None);
    assert!(
        matches!(twice.attention(NOW), Attention::Digest(n) if n.kind == "repeating-stop"),
        "{:?}",
        twice.attention(NOW)
    );
    for phase in [
        Phase::Pending,
        Phase::Announced {
            at_s: NOW - 60,
            asks: 1,
        },
    ] {
        let rearmed = Row {
            phase,
            retry_at: 0,
            wait: String::new(),
            ..twice.clone()
        };
        assert_eq!(rearmed.stall(NOW).as_deref(), Some("failed:signal-refused"));
        let first = Row {
            stop_streak: 1,
            behind_since: NOW - 60,
            ..rearmed
        };
        assert_eq!(first.stall(NOW), None);
    }

    let refused = Row {
        phase: Phase::Failed("not-a-shell-job".into()),
        stop_streak: 1,
        streak_why: "not-a-shell-job".into(),
        ..base
    };
    assert!(!refused.asks_on_its_own(NOW) && refused.badge(NOW).is_none());
    assert!(
        matches!(refused.attention(NOW), Attention::Digest(n) if n.kind == "refused-launch"),
        "a refusal is the owner's launch choice: a digest line"
    );
    assert_eq!(
        refused.column(NOW),
        "stalled/2.1.282/refused:not-a-shell-job/8h"
    );
}

#[test]
fn a_person_held_announced_upgrade_is_a_record_never_a_mark() {
    for wait in [
        "draft",
        "box",
        "held",
        "login",
        "not-idle:waiting",
        "release:draft",
    ] {
        let row = Row {
            phase: Phase::Announced {
                at_s: NOW - 3_600,
                asks: 1,
            },
            wait: wait.into(),
            ..pending(3 * 3_600 + STALLED_AFTER_S)
        };
        assert_eq!(row.stall(NOW).as_deref(), Some("overdue"), "{wait}");
        assert!(!row.asks_on_its_own(NOW), "{wait}: only a person moves it");
        assert_eq!(row.badge(NOW), None, "{wait}: a person's hold marks no tab");
        assert_eq!(row.attention(NOW), Attention::Quiet, "{wait}");
    }
    let draft = Row {
        phase: Phase::Announced {
            at_s: NOW - 3_600,
            asks: 1,
        },
        wait: "draft".into(),
        ..pending(30 * 3_600)
    };
    assert!(!draft.asks_on_its_own(NOW));
    assert_eq!(draft.badge(NOW), None);
    assert!(
        matches!(draft.attention(NOW), Attention::Digest(n) if n.kind == "typed-draft"),
        "{:?}",
        draft.attention(NOW)
    );
    for wait in ["background", "awaiting-ready", "limited"] {
        let row = Row {
            phase: Phase::Announced {
                at_s: NOW - 600,
                asks: 1,
            },
            wait: wait.into(),
            wait_since: NOW - 600,
            ..pending(STALLED_AFTER_S + 3_600)
        };
        assert!(row.asks_on_its_own(NOW), "{wait}");
        assert_eq!(row.badge(NOW), None, "{wait}");
        assert_eq!(row.attention(NOW), Attention::Quiet, "{wait}");
    }
    for (wait_age, ask_age) in [(60, 600), (60, 86_400), (86_400, 600)] {
        let row = Row {
            phase: Phase::Announced {
                at_s: NOW - ask_age,
                asks: 1,
            },
            wait: "draft".into(),
            wait_since: NOW - wait_age,
            ..pending(7 * 3_600)
        };
        assert!(row.asks_on_its_own(NOW), "{wait_age}/{ask_age}");
        assert_eq!(row.badge(NOW), None, "{wait_age}/{ask_age}");
        let stands = (NOW - wait_age).max(NOW - ask_age) + upgrade::REASK_S;
        assert_eq!(
            next_change(std::slice::from_ref(&row), NOW),
            Some(stands),
            "{wait_age}/{ask_age}"
        );
        assert!(!row.asks_on_its_own(stands), "{wait_age}/{ask_age}: stood");
    }
}

#[test]
fn a_day_behind_is_news_whatever_the_rounds_do() {
    let gave_up = Row {
        phase: Phase::Failed(upgrade::GAVE_UP.into()),
        streak_why: upgrade::GAVE_UP.into(),
        stop_streak: 5,
        wait: "failed".into(),
        retry_at: NOW + 600,
        ..pending(3 * 86_400)
    };
    let asking = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 3,
        },
        wait: "background".into(),
        ..pending(3 * 86_400)
    };
    let first_stop = Row {
        phase: Phase::Failed("signal-refused".into()),
        stop_streak: 1,
        streak_why: "signal-refused".into(),
        wait: "failed".into(),
        ..pending(3 * 86_400)
    };
    let repeating = Row {
        phase: Phase::Failed("no-resume".into()),
        stop_streak: 2,
        streak_why: "no-resume".into(),
        ..first_stop.clone()
    };
    let held = Row {
        phase: Phase::Announced {
            at_s: NOW - 86_400,
            asks: 1,
        },
        wait: "draft".into(),
        ..pending(3 * 86_400)
    };
    let limited = Row {
        wait: "limited".into(),
        ..asking.clone()
    };
    for (why, row, asks, again) in [
        ("five rounds given up", &gave_up, false, true),
        ("a round still asking", &asking, false, true),
        ("a first stop", &first_stop, false, true),
        ("a stop that repeats", &repeating, false, false),
        ("a person's hold that stood", &held, false, false),
    ] {
        assert_eq!(row.asks_on_its_own(NOW), asks, "{why}");
        assert_eq!(row.will_ask_again(NOW), again, "{why}");
    }
    assert!(limited.asks_on_its_own(NOW), "a limit ends by itself");
    let young = Row {
        behind_since: NOW - 8 * 3_600,
        ..gave_up
    };
    let turned = young.behind_since + MOVE_BUDGET_S;
    assert!(young.asks_on_its_own(NOW));
    assert_eq!(next_change(std::slice::from_ref(&young), NOW), Some(turned));
    assert!(!young.asks_on_its_own(turned));
}

#[test]
fn a_limit_whose_named_reset_has_passed_is_told() {
    let passed = NOW - LIMIT_GRACE_S - 3 * 3_600;
    let at = |limit_until: u64| Row {
        wait: "limited".into(),
        limit_until,
        ..pending(3_600)
    };
    let row = at(passed);
    assert!(row.limit_passed(NOW));
    assert!(!row.asks_on_its_own(NOW));
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    let Attention::Fault(need) = row.attention(NOW) else {
        panic!("{:?}", row.attention(NOW));
    };
    assert_eq!(need.kind, "limit-passed");
    assert!(need.why.chars().count() <= NEED_WHY_MAX, "{need:?}");
    assert!(need.then.chars().count() <= NEED_THEN_MAX, "{need:?}");
    assert_eq!(row.cause_word(NOW), "limit-passed");
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 60,
        ..row.clone()
    };
    assert!(
        matches!(hurried.attention(NOW), Attention::Fault(_)),
        "the word does not hide it"
    );
    let moved_on = Row {
        wait: "not-idle:busy".into(),
        ..row.clone()
    };
    assert!(!moved_on.limit_passed(NOW) && moved_on.attention(NOW) == Attention::Quiet);
    let unnamed = at(0);
    assert_eq!(unnamed.cause_word(NOW), "limit");
    assert_ne!(unnamed.wait_since, 0, "the fixture's wait has a start");
    let bound = unnamed.wait_since + LONGEST_LIMIT_S + LIMIT_GRACE_S;
    assert_eq!(
        unnamed.attention(bound - 1),
        Attention::Quiet,
        "never before any limit can have ended"
    );
    assert!(
        matches!(unnamed.attention(bound), Attention::Fault(_)),
        "told at the bound"
    );
    assert_eq!(unnamed.next_attention_at(NOW), Some(bound), "and named");
    let far = bound + 30 * 86_400;
    let unknown = Row {
        wait_since: 0,
        ..unnamed.clone()
    };
    assert!(!unknown.limit_passed(far), "an unknown start");
    let ahead = Row {
        limit_until: far + 3_600,
        ..unnamed.clone()
    };
    assert!(!ahead.limit_passed(far), "the named reset wins");

    let st = St {
        phase: Phase::Pending,
        to: "2.1.283".into(),
        wait: "limited".into(),
        pending_since: NOW - 3_600,
        limit_until: passed,
        ..St::default()
    };
    let back = St::from_json(&st.to_json()).expect("parses");
    let row = Row::of("s", &back, NOW);
    assert_eq!(row.limit_until, passed);
    assert!(matches!(row.attention(NOW), Attention::Fault(_)));
    assert_eq!(
        row.to_json(NOW).get("limit_until").and_then(Value::as_u64),
        Some(passed)
    );
}

#[test]
fn status_says_what_holds_the_move_and_what_the_owner_is_told() {
    let words = |row: &Row| {
        let line = row.line(NOW);
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .unwrap_or_else(|| panic!("{key} in {line}"))
                .to_string()
        };
        let json = row.to_json(NOW);
        let j = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(field("cause="), j("cause"), "{line}");
        assert_eq!(field("told="), j("told"), "{line}");
        assert!(
            !j("cause_words").is_empty() || field("cause=") == "-",
            "{line}"
        );
        (field("cause="), field("told="))
    };
    let pair = |c: &str, t: &str| (c.to_string(), t.to_string());
    assert_eq!(words(&pending(3_600)), pair("own-work", "-"));
    assert_eq!(words(&pending(STALLED_AFTER_S + 60)), pair("own-work", "-"));
    assert_eq!(
        words(&pending(MOVE_BUDGET_S + 60)),
        pair("own-work", "own-work")
    );
    let boxed = Row {
        wait: "box".into(),
        ..pending(STALLED_AFTER_S + 60)
    };
    assert_eq!(words(&boxed), pair("person", "-"));
    let refused = Row {
        wait: "announce-refused:changed".into(),
        refused: 3,
        refused_at: NOW - TELL_S,
        held_by: vec![HeldBy {
            pid: 51_435,
            name: "zsh".into(),
            since: NOW - 86_400,
        }],
        ..pending(3_600)
    };
    assert_eq!(words(&refused), pair("own-work", "harness-stuck"));
    let stopped = Row {
        phase: Phase::Failed("signal-refused".into()),
        stop_streak: 2,
        streak_why: "signal-refused".into(),
        wait: "failed".into(),
        ..pending(3_600)
    };
    assert_eq!(words(&stopped).1, "repeating-stop");
    let done = Row {
        phase: Phase::Done,
        ..pending(3_600)
    };
    assert_eq!(words(&done), pair("-", "-"));
    let odd = Row {
        wait: "something-new".into(),
        ..pending(3_600)
    };
    assert_eq!(words(&odd), pair("unclassified", "-"));
}

#[test]
fn a_stopped_refusal_is_typed_for_the_window_and_a_sentence_for_the_shell() {
    let e = format!("{REFUSED_STOPPED}1790318276:the upgrade in tab s-a stopped (no-resume)");
    assert_eq!(
        refused_stopped(&e),
        Some((1_790_318_276, "the upgrade in tab s-a stopped (no-resume)"))
    );
    assert_eq!(
        refusal_sentence(&e),
        "the upgrade in tab s-a stopped (no-resume)"
    );
    assert_eq!(refusal_sentence("busy:another-sweep"), "busy:another-sweep");
    assert_eq!(refused_stopped("stale:2.1.283"), None);
}

const WAITS: &[&str] = &[
    "",
    "settling",
    "attended",
    "busy",
    "not-idle:busy",
    "not-idle:shell",
    "not-idle:waiting",
    "not-idle",
    "status-stale:busy",
    "held",
    "limited",
    "login",
    "queued",
    "draft",
    "box",
    "background",
    "awaiting-ready",
    "release:awaiting-ready",
    "announce-refused:changed",
    "announce-refused:yield",
    "announce-refused:held",
    "changed",
    "changed-before-signal",
    "signal-refused",
    "notice-owned-by-other-process",
    "tasked",
    "self",
    "not-foreground",
    "ids",
    "terminal:tmux",
    "terminal:none",
    "left-typed:x",
    "thread-locked",
    "in-flight",
    "screen-unreadable",
    "threads-ambiguous",
    "files-unreadable",
    "session-files-unreadable",
    "shell-dialect",
    "no-process",
    "argv-unreadable",
    "exe-unreadable",
    "version-unreadable",
    "home-unreadable",
    "tab-not-live",
    "no-socket",
    "conversation-in-other-tab",
];

#[test]
fn every_wait_has_a_horizon_and_none_interrupts_by_age() {
    for agent in [upgrade::Agent::Claude, upgrade::Agent::Codex] {
        for phase in [Phase::Pending, Phase::Announced { at_s: NOW, asks: 1 }] {
            for wait in WAITS {
                let row = |ago: u64| Row {
                    agent,
                    phase: match phase {
                        Phase::Announced { asks, .. } => Phase::Announced {
                            at_s: NOW - ago + 60,
                            asks,
                        },
                        _ => Phase::Pending,
                    },
                    wait: (*wait).to_string(),
                    wait_since: NOW - 60,
                    minted_at: NOW - ago,
                    held_by: vec![HeldBy {
                        pid: 7,
                        name: "zsh".into(),
                        since: NOW - 600,
                    }],
                    ..pending(ago)
                };
                let at = format!("{agent:?} {phase:?} wait={wait:?}");
                match row(3_600).attention(NOW) {
                    Attention::Quiet => {}
                    Attention::Digest(n) if matches!(n.kind, "no-terminal" | "no-integration") => {}
                    other => panic!("an hour old and already loud: {at}: {other:?}"),
                }
                match row(100 * 86_400).attention(NOW) {
                    Attention::Quiet => panic!("quiet for a hundred days: {at}"),
                    Attention::Interrupt(n) => panic!("a wait interrupts nobody: {at}: {n:?}"),
                    Attention::Fault(n) => assert!(
                        matches!(
                            n.kind,
                            "no-progress" | "harness-stuck" | "daemon-update-failing"
                        ),
                        "{at}: {}",
                        n.kind
                    ),
                    Attention::Digest(n) => {
                        assert!(!n.then.is_empty() && !n.why.is_empty(), "{at}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_quiet_wait_turns_a_fault_line_at_its_own_horizon() {
    let row = |wait: &str, ago: u64| Row {
        wait: wait.to_string(),
        wait_since: NOW - 60,
        minted_at: NOW - ago,
        ..pending(ago)
    };
    for (wait, bound) in [
        ("box", BACKSTOP_S),
        ("login", BACKSTOP_S),
        ("held", BACKSTOP_S),
        ("attended", BACKSTOP_S),
        ("not-idle:waiting", BACKSTOP_S),
        ("limited", LIMIT_MAX_S),
        ("queued", QUEUED_MAX_S),
    ] {
        assert_eq!(
            row(wait, bound - 60).attention(NOW),
            Attention::Quiet,
            "{wait}: within its horizon"
        );
        let Attention::Fault(n) = row(wait, bound + 60).attention(NOW) else {
            panic!("{wait}: past its horizon");
        };
        assert_eq!(n.kind, "no-progress", "{wait}");
        assert_eq!(n.since, NOW - bound - 60, "{wait}: counted from the start");
        assert_eq!(
            row(wait, bound - 60).next_attention_at(NOW),
            Some(NOW + 60),
            "{wait}: the host wakes at the horizon"
        );
    }
    let asked = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        noticed_at: NOW - 600,
        ..row("box", 100 * 86_400)
    };
    assert_eq!(asked.attention(NOW), Attention::Quiet);
    let live = Row {
        minted_at: 0,
        ..row("box", 100 * 86_400)
    };
    assert_eq!(live.attention(NOW), Attention::Quiet);
    assert_eq!(live.next_attention_at(NOW), None);
}

#[test]
fn the_agents_own_work_is_the_rounds_not_the_last_looks() {
    let digest =
        |r: &Row| matches!(r.attention(NOW), Attention::Digest(ref n) if n.kind == "own-work");
    let zsh = HeldBy {
        pid: 51_435,
        name: "zsh".to_string(),
        since: NOW - 3 * 3_600,
    };
    let round = Row {
        phase: Phase::Failed(upgrade::GAVE_UP.into()),
        streak_why: upgrade::GAVE_UP.into(),
        stop_streak: 2,
        own_work_at: NOW - 60,
        ..pending(27 * 3_600)
    };
    let looked = Row {
        wait: "failed".into(),
        held_by: vec![zsh],
        ..round.clone()
    };
    assert!(digest(&looked));
    let acted = Row {
        wait: String::new(),
        ..round.clone()
    };
    assert!(digest(&acted), "an act is no end of the work");
    for wait in ["box", "draft", "login", "held", "attended"] {
        let between = Row {
            wait: wait.into(),
            ..round.clone()
        };
        assert!(digest(&between), "{wait}: between two looks at the work");
    }
    let limited = Row {
        wait: "limited".into(),
        ..round.clone()
    };
    assert!(!digest(&limited));
    let stale = Row {
        own_work_at: NOW - OWN_WORK_HOLD_S - 60,
        ..acted.clone()
    };
    assert!(!digest(&stale), "an hour after the last look that saw it");
    assert_eq!(
        acted.next_attention_at(NOW),
        Some(NOW - 60 + OWN_WORK_HOLD_S),
        "the host wakes when the memory runs out"
    );
}

#[test]
fn only_an_agent_the_upgrade_ended_interrupts() {
    let stale = super::super::STALE_S;
    let interrupt = |r: &Row| match r.attention(NOW) {
        Attention::Interrupt(n) => Some(n.kind),
        _ => None,
    };
    let base = pending(7 * 3_600);
    for (why, row, want) in [
        (
            "stopped after the exit",
            Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                ..base.clone()
            },
            Some("agent-down"),
        ),
        (
            "no-resume after the exit",
            Row {
                phase: Phase::Failed("no-resume".into()),
                exited_at: NOW - 600,
                ..base.clone()
            },
            Some("agent-down"),
        ),
        (
            "gone, its resume line not typed past the bound",
            Row {
                phase: Phase::Exiting {
                    at_s: NOW - stale - 60,
                },
                exited_at: NOW - stale - 30,
                ..base.clone()
            },
            Some("agent-down"),
        ),
        (
            "asked to end, alive three bounds on",
            Row {
                phase: Phase::Exiting {
                    at_s: NOW - HUNG_EXIT_AFTER_S - 1,
                },
                ..base.clone()
            },
            Some("hung-exit"),
        ),
        (
            "asked to end, alive within three bounds",
            Row {
                phase: Phase::Exiting {
                    at_s: NOW - stale - 60,
                },
                ..base.clone()
            },
            None,
        ),
        (
            "gone, within the bound",
            Row {
                phase: Phase::Exiting { at_s: NOW - 60 },
                exited_at: NOW - 30,
                ..base.clone()
            },
            None,
        ),
        (
            "relaunched, no idle point yet",
            Row {
                phase: Phase::Relaunched {
                    at_s: NOW - 3 * stale,
                },
                exited_at: NOW - 3 * stale,
                ..base.clone()
            },
            None,
        ),
        (
            "a first stop with the agent alive",
            Row {
                phase: Phase::Failed("signal-refused".into()),
                stop_streak: 1,
                streak_why: "signal-refused".into(),
                ..base.clone()
            },
            None,
        ),
        (
            "a refusal",
            Row {
                phase: Phase::Failed("not-a-shell-job".into()),
                ..base.clone()
            },
            None,
        ),
        (
            "a round that gave up",
            Row {
                phase: Phase::Failed(upgrade::GAVE_UP.into()),
                stop_streak: 3,
                streak_why: upgrade::GAVE_UP.into(),
                ..base.clone()
            },
            None,
        ),
        (
            "done",
            Row {
                phase: Phase::Done,
                ..base.clone()
            },
            None,
        ),
        (
            "the owner's skip of an agent that is down",
            Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 600,
                request: Request::Skip("2.1.282".into()),
                ..base.clone()
            },
            None,
        ),
    ] {
        assert_eq!(interrupt(&row), want, "{why}");
        if want.is_some() {
            let stall = row.stall(NOW);
            assert!(
                stall.is_some() && stall.as_deref() != Some("overdue"),
                "{why}: {stall:?}"
            );
        }
    }
    let down = Row {
        phase: Phase::Failed("stale-exit".into()),
        exited_at: NOW - 600,
        ..base
    };
    let Attention::Interrupt(need) = down.attention(NOW) else {
        panic!("{:?}", down.attention(NOW));
    };
    assert_eq!(need.severity, NeedSeverity::Error);
    assert_eq!(
        need.command.as_deref(),
        Some("claude --resume 03396a15-856e-4f1b-8174-ae9a3e4b369f")
    );
    assert_eq!(need.fingerprint, format!("agent-down/{TAB}"));
}

#[test]
fn a_whole_round_never_flips_the_owners_line_at_its_acts() {
    let round = |seen: bool| {
        let held = if seen {
            vec![upgrade::Held {
                pid: 4000,
                name: "zsh".into(),
                age_s: 600,
                command: String::new(),
            }]
        } else {
            Vec::new()
        };
        let work = if seen { "wait:background" } else { "wait:box" };
        let resting = if seen { "wait:failed" } else { "wait:box" };
        let t0 = NOW;
        let mut st = St {
            from: "2.1.283".into(),
            to: "2.1.284".into(),
            tab: TAB.into(),
            pending_since: t0 - 30 * 3_600,
            salt: t0 - 30 * 3_600,
            streak_why: upgrade::GAVE_UP.into(),
            stop_streak: 1,
            failed_at: t0 - 100,
            phase: Phase::Failed(upgrade::GAVE_UP.into()),
            ..St::default()
        };
        let mut classes: Vec<String> = Vec::new();
        let mut record = |st: &St, now: u64| {
            classes.push(match Row::of("c", st, now).attention(now) {
                Attention::Quiet => "quiet".to_string(),
                Attention::Fault(n) => format!("fault:{}", n.kind),
                Attention::Digest(n) => format!("digest:{}", n.kind),
                Attention::Interrupt(n) => format!("interrupt:{}", n.kind),
            });
        };
        let mut now = t0;
        st.note_step(resting, "busy", now);
        st.note_held(resting, &held, now);
        record(&st, now);
        for k in 1..12u64 {
            st.note_step(resting, "busy", t0 + 600 * k);
            st.note_held(resting, &held, t0 + 600 * k);
            record(&st, t0 + 600 * k + 1);
        }
        now = t0 + 7_200;
        let _ = st.rearm(now);
        st.note_step("rearmed:unanswered", "busy", now);
        st.note_held("rearmed:unanswered", &[], now);
        record(&st, now + 1);
        for ask in 1..=4u32 {
            now += 60;
            st.note_step(work, "busy", now);
            st.note_held(work, &held, now);
            record(&st, now + 1);
            now += 30;
            st.announced(format!("M{ask}"), now, ask);
            st.note_step(&format!("announced:{ask}"), "busy", now);
            st.note_held(&format!("announced:{ask}"), &[], now);
            record(&st, now + 1);
            now += 60;
            st.note_step(work, "busy", now);
            st.note_held(work, &held, now);
            record(&st, now + 1);
            now += 1_740;
        }
        st.give_up(now);
        st.note_step("gave-up", "busy", now);
        st.note_held("gave-up", &[], now);
        record(&st, now + 1);
        st.released();
        st.note_step("released:gave-up", "busy", now + 5);
        st.note_held("released:gave-up", &[], now + 5);
        record(&st, now + 6);
        for k in 1..=3u64 {
            st.note_step(resting, "busy", now + 60 * k);
            st.note_held(resting, &held, now + 60 * k);
            record(&st, now + 60 * k + 1);
        }
        classes
    };
    let seen = round(true);
    assert!(
        seen.iter().all(|c| c == "digest:own-work"),
        "the line never dips at an act: {seen:?}"
    );
    let unseen = round(false);
    assert!(
        unseen.iter().all(|c| c != "digest:own-work"),
        "no work seen, no work line: {unseen:?}"
    );
}

#[test]
fn the_agents_own_work_is_news_only_after_a_day_and_a_round() {
    let round = |behind: u64, rounds: u32, works: bool| Row {
        phase: Phase::Failed(upgrade::GAVE_UP.into()),
        stop_streak: rounds,
        streak_why: if rounds > 0 {
            upgrade::GAVE_UP.into()
        } else {
            String::new()
        },
        wait: "failed".into(),
        held_by: if works {
            (0..16)
                .map(|i| HeldBy {
                    pid: 4000 + i,
                    name: "zsh".into(),
                    since: NOW - 600,
                })
                .collect()
        } else {
            Vec::new()
        },
        ..pending(behind)
    };
    let digest = |r: &Row| match r.attention(NOW) {
        Attention::Digest(n) => Some(n),
        _ => None,
    };
    for (why, r) in [
        ("within the first day", round(DIGEST_AFTER_S - 1, 2, true)),
        (
            "no whole round answered not yet",
            round(30 * 3_600, 0, true),
        ),
    ] {
        assert_eq!(r.attention(NOW), Attention::Quiet, "{why}");
    }
    let bare_row = round(30 * 3_600, 2, false);
    let bare = digest(&bare_row).expect("a round given up on");
    assert!(bare_row.more_lines(&bare, NOW)[0].contains(" 8 "));
    let news_row = round(DIGEST_AFTER_S, 2, true);
    let news = digest(&news_row).expect("a day, a round, work");
    assert_eq!((news.kind, news.severity), ("own-work", NeedSeverity::Info));
    let more = news_row.more_lines(&news, NOW);
    assert!(more[0].contains(" 8 "), "{more:?}");
    assert!(more.len() <= 3 && more.iter().all(|l| l.chars().count() <= 200));
    assert!(!news.why.contains("pid") && !news.then.contains("pid"));
    assert!(news.log_line.contains("4000"), "{}", news.log_line);
    let asking = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 2,
        },
        wait: "background".into(),
        ..round(30 * 3_600, 1, true)
    };
    let news = digest(&asking).expect("announced, a round behind it");
    assert!(asking.more_lines(&news, NOW)[0].contains(" 6 "));
}

#[test]
fn a_record_an_older_build_wrote_is_aged_from_its_minting_until_a_visit_settles_it() {
    let (minted, carried, at) = (1_790_618_538, 1_790_373_026, 1_790_658_000);
    let legacy = St {
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: carried,
        salt: minted,
        wait: "draft".to_string(),
        wait_since: 1_790_640_516,
        ..St::default()
    };
    let row = Row::of("s", &legacy, at);
    assert_eq!(row.behind_since, minted, "not three days");
    assert!(
        row.column(at).ends_with("/10h57m"),
        "the age the owner reads: {}",
        row.column(at)
    );
    for (why, st) in [
        (
            "it names its process",
            St {
                proc_start: "Sun Sep 27 16:20:11 2026".to_string(),
                ..legacy.clone()
            },
        ),
        (
            "it keeps a wait log",
            St {
                waits: vec![super::super::WaitNote {
                    at: 1_790_640_516,
                    word: "draft".to_string(),
                    note: String::new(),
                }],
                ..legacy.clone()
            },
        ),
        (
            "its age is within its own life",
            St {
                pending_since: minted + 60,
                ..legacy.clone()
            },
        ),
    ] {
        assert_eq!(
            Row::of("s", &st, at).behind_since,
            st.behind_since(),
            "{why}: read as it stands"
        );
    }
}

#[test]
fn a_restart_a_hand_holds_is_not_said_down_until_its_clock_runs_out() {
    let stale = super::super::STALE_S;
    let kind = |r: &Row, at: u64| match r.attention(at) {
        Attention::Interrupt(n) => Some(n.kind),
        _ => None,
    };
    let held = Row {
        phase: Phase::Exiting { at_s: NOW - 900 },
        exited_at: NOW - 900,
        hand_seen: NOW - 10,
        wait: "held".into(),
        ..pending(7 * 3_600)
    };
    assert_eq!(kind(&held, NOW), None, "held: {:?}", held.attention(NOW));
    let at = NOW - 10 + stale + 1;
    assert_eq!(held.next_attention_at(NOW), Some(at));
    assert_eq!(kind(&held, at), Some("agent-down"));
    let unheld = Row {
        hand_seen: 0,
        ..held.clone()
    };
    assert_eq!(kind(&unheld, NOW), Some("agent-down"));
}

#[test]
fn next_attention_at_names_every_change_by_time() {
    let phases = [
        Phase::Pending,
        Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        Phase::Failed(upgrade::GAVE_UP.into()),
        Phase::Failed("signal-refused".into()),
        Phase::Relaunched { at_s: NOW - 600 },
        Phase::Exiting { at_s: NOW - 100 },
    ];
    let horizon = 12 * 86_400;
    let step = 600;
    let mut rows = 0;
    for agent in [upgrade::Agent::Claude, upgrade::Agent::Codex] {
        for phase in &phases {
            for wait in WAITS.iter().step_by(3) {
                for held in [false, true] {
                    let exits: &[(u64, u64)] = if matches!(phase, Phase::Exiting { .. }) {
                        &[(0, 0), (NOW - 100, 0), (NOW - 100, NOW - 30)]
                    } else {
                        &[(0, 0)]
                    };
                    for ((ago, remembered), (exited_at, hand_seen)) in
                        [(3_600, 0), (30 * 3_600, NOW - 600), (9 * 86_400, 0)]
                            .into_iter()
                            .flat_map(|age| exits.iter().map(move |exit| (age, *exit)))
                    {
                        let row = Row {
                            agent,
                            phase: phase.clone(),
                            exited_at,
                            hand_seen,
                            wait: (*wait).to_string(),
                            wait_since: NOW - 60,
                            minted_at: NOW - ago,
                            own_work_at: remembered,
                            streak_why: upgrade::GAVE_UP.to_string(),
                            stop_streak: 2,
                            held_by: if held {
                                vec![HeldBy {
                                    pid: 7,
                                    name: "zsh".into(),
                                    since: NOW - 600,
                                }]
                            } else {
                                Vec::new()
                            },
                            ..pending(ago)
                        };
                        rows += 1;
                        let here = row.attention(NOW).level();
                        let named = row.next_attention_at(NOW);
                        let first = (1..=horizon / step)
                            .map(|k| NOW + k * step)
                            .find(|t| row.attention(*t).level() != here);
                        let at = format!(
                            "{agent:?} {phase:?} wait={wait:?} held={held} ago={ago} \
                             exited={exited_at} hand={hand_seen}"
                        );
                        match (named, first) {
                            (Some(n), _) => {
                                assert!(n > NOW, "{at}: in the future");
                                assert_ne!(row.attention(n).level(), here, "{at}: a real change");
                                if let Some(f) = first {
                                    assert!(n <= f, "{at}: named {n}, differs by {f}");
                                }
                            }
                            (None, Some(f)) => panic!("{at}: unnamed, but differs by {f}"),
                            (None, None) => {}
                        }
                    }
                }
            }
        }
    }
    assert!(rows > 500, "{rows} rows swept");
}

#[cfg(unix)]
#[test]
fn a_mark_on_a_tab_left_as_it_stands_stays() {
    let dir = ScratchDir::new("upgrade-status", "kept-mark");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let waiting = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 60,
        ..St::default()
    };
    let stalled = St {
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: now_s() - 600,
        ..waiting.clone()
    };
    save(&o, "aaa", &stalled);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let lowered = |asked: &std::sync::Mutex<Vec<String>>| -> bool {
        asked.lock().is_ok_and(|a| {
            a.iter()
                .any(|l| *l == format!("@{TAB} meta unset attention owner=upgrade"))
        })
    };
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let held = behind_in(TAB);
    let mut view = View::default();
    let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    assert!(
        view.badged.contains_key(TAB),
        "the agent the upgrade ended is marked"
    );
    let looped = state_dir(&o).join("bbb.json");
    std::os::unix::fs::symlink(&looped, &looped).expect("a record that loops");
    save(&o, "aaa", &waiting);
    asked.lock().expect("log").clear();
    let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    assert!(
        !lowered(&asked),
        "left as it stands: its mark is not lowered"
    );
    assert!(view.badged.contains_key(TAB));
    std::fs::remove_file(&looped).expect("unloop");
    let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    assert!(lowered(&asked), "CONTROL: read whole, the mark is lowered");
}

#[test]
fn a_digest_line_is_keyed_by_kind_and_tab_and_says_no_restart() {
    let mut swept = 0;
    for agent in [upgrade::Agent::Claude, upgrade::Agent::Codex] {
        for wait in WAITS {
            for (phase, streak_why, rounds) in [
                (Phase::Pending, "", 0),
                (Phase::Announced { at_s: NOW, asks: 1 }, upgrade::GAVE_UP, 2),
                (Phase::Failed("not-a-shell-job".into()), "", 0),
                (
                    Phase::Failed("line:a control character in the line".into()),
                    "",
                    0,
                ),
                (Phase::Failed("argv:--frob".into()), "", 0),
                (Phase::Failed("signal-refused".into()), "signal-refused", 3),
                (Phase::Failed("stale-exit".into()), "", 0),
                (Phase::Exiting { at_s: 0 }, "", 0),
            ] {
                let exited = if matches!(&phase, Phase::Failed(w) if w == "stale-exit") {
                    NOW - 600
                } else {
                    0
                };
                let mk = |to: &str, behind: u64, tab: &str| Row {
                    agent,
                    phase: phase.clone(),
                    wait: (*wait).to_string(),
                    wait_since: NOW - 30 * 3_600,
                    exited_at: exited,
                    to: to.to_string(),
                    tab: tab.to_string(),
                    stop_streak: rounds,
                    streak_why: streak_why.to_string(),
                    held_by: vec![HeldBy {
                        pid: 7,
                        name: "zsh".into(),
                        since: NOW - 600,
                    }],
                    ..pending(behind)
                };
                let (a, b) = (
                    mk("2.1.284", 30 * 3_600, TAB),
                    mk("2.1.290", 90 * 3_600, TAB),
                );
                let (na, nb) = match (a.attention(NOW), b.attention(NOW)) {
                    (
                        Attention::Fault(x) | Attention::Digest(x) | Attention::Interrupt(x),
                        Attention::Fault(y) | Attention::Digest(y) | Attention::Interrupt(y),
                    ) => (x, y),
                    _ => continue,
                };
                let fault = matches!(a.attention(NOW), Attention::Fault(_));
                assert_eq!(na.fingerprint, nb.fingerprint, "{agent:?} {wait} {phase:?}");
                assert_eq!(na.kind, nb.kind);
                let other = mk("2.1.284", 30 * 3_600, "s-other");
                if let Attention::Fault(o) | Attention::Digest(o) | Attention::Interrupt(o) =
                    other.attention(NOW)
                {
                    assert_ne!(o.fingerprint, na.fingerprint, "another tab is its own line");
                }
                let placed = format!("{} in tab 12", na.title);
                let words = placed
                    .split_whitespace()
                    .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
                    .count();
                assert!(words <= 6, "{placed}");
                assert!(placed.chars().count() <= 48, "{placed}");
                assert!(
                    !placed.contains(" — ") && !placed.contains("; ") && !placed.contains(": "),
                    "{placed}"
                );
                assert!(!placed.ends_with('.'), "{placed}");
                assert!(na.why.chars().count() <= NEED_WHY_MAX, "{}", na.why);
                assert!(na.then.chars().count() <= NEED_THEN_MAX, "{}", na.then);
                assert!(
                    !na.why.contains('…') && !na.then.contains('…'),
                    "cut: {na:?}"
                );
                assert!(!na.log_line.contains('\n') && !na.log_line.is_empty());
                assert!(!na.why.contains('\n') && !na.then.contains('\n'));
                if !fault {
                    assert!(
                        !na.then.contains("; ") && !na.then.contains(" or "),
                        "{}: {}",
                        na.kind,
                        na.then
                    );
                }
                swept += 1;
            }
        }
    }
    assert!(swept > 100, "{swept} lines swept");
}

#[cfg(unix)]
#[test]
fn a_row_that_crosses_a_horizon_is_a_changed_row() {
    let dir = ScratchDir::new("upgrade-status", "horizon");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let waiting = |wait: &str| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now - 2 * 3_600,
        wait: wait.to_string(),
        wait_since: now - SUSPENDED_AFTER_S + 100,
        ..St::default()
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    for (wait, at_horizon) in [
        ("not-foreground", Level::Digest("suspended")),
        ("limited", Level::Quiet),
    ] {
        save(&o, "aaa", &waiting(wait));
        let mut view = View::default();
        let sent: std::cell::RefCell<Vec<Vec<Level>>> = std::cell::RefCell::default();
        let look = |view: &mut View, at: u64| {
            view.refresh_with(&o, &tabs, at, &held, &mut |rows| {
                sent.borrow_mut()
                    .push(rows.iter().map(|r| r.level).collect());
            })
        };
        let next = look(&mut view, now);
        if at_horizon == Level::Quiet {
            let watch = super::super::load(&o, "aaa")
                .and_then(|st| super::super::watch_at(&st))
                .unwrap_or(u64::MAX);
            assert_eq!(
                next,
                Some((now - 2 * 3_600 + STALLED_AFTER_S).min(watch)),
                "{wait}"
            );
        } else {
            assert_eq!(next, Some(now + 100), "{wait}: woken at the horizon");
        }
        let _ = look(&mut view, now + 50);
        assert_eq!(
            *sent.borrow(),
            [vec![Level::Quiet]],
            "{wait}: unchanged, sent once"
        );
        let _ = look(&mut view, now + 101);
        if at_horizon == Level::Quiet {
            assert_eq!(
                sent.borrow().len(),
                1,
                "{wait}: nothing crossed, nothing sent"
            );
        } else {
            assert_eq!(
                *sent.borrow(),
                [vec![Level::Quiet], vec![at_horizon]],
                "{wait}: the horizon is a changed row"
            );
        }
    }
}

#[test]
fn aterms_own_fault_is_one_log_line_never_a_digest_line_or_a_mark() {
    let at = |agent, wait: &str, waited: u64| Row {
        agent,
        wait: wait.to_string(),
        wait_since: NOW - waited,
        ..pending(waited.max(6 * 3_600))
    };
    for (agent, wait) in [
        (upgrade::Agent::Claude, "announce-refused:changed"),
        (upgrade::Agent::Claude, "changed"),
        (upgrade::Agent::Claude, "signal-refused"),
        (upgrade::Agent::Claude, "tasked"),
        (upgrade::Agent::Claude, "self"),
        (upgrade::Agent::Claude, "ids"),
        (upgrade::Agent::Claude, "not-idle"),
        (upgrade::Agent::Claude, "settling"),
        (upgrade::Agent::Claude, "awaiting-ready"),
        (upgrade::Agent::Claude, "tab-not-live"),
    ] {
        let young = at(agent, wait, HARNESS_TELL_S - 60);
        assert_eq!(young.attention(NOW), Attention::Quiet, "{wait}: young");
        let old = at(agent, wait, HARNESS_TELL_S + 60);
        assert!(
            matches!(old.attention(NOW), Attention::Fault(ref n) if n.kind == "harness-stuck"),
            "{wait}: {:?}",
            old.attention(NOW)
        );
        assert_eq!(old.badge(NOW), None, "{wait}: a fault marks no tab");
        let days = at(agent, wait, 5 * 86_400);
        assert!(matches!(days.attention(NOW), Attention::Fault(_)), "{wait}");
        assert_eq!(
            old.next_attention_at(NOW - 3_600),
            Some(old.wait_since + HARNESS_TELL_S),
            "{wait}"
        );
    }
    let minted = |minted_ago: u64, phase: Phase, wait: &str| Row {
        phase,
        wait: wait.to_string(),
        wait_since: if wait.is_empty() { 0 } else { NOW - 60 },
        minted_at: NOW - minted_ago,
        ..pending(100 * 86_400)
    };
    assert_eq!(
        minted(HARNESS_TELL_S - 60, Phase::Pending, "").attention(NOW),
        Attention::Quiet,
        "a record only just minted has had no look yet"
    );
    assert!(
        matches!(
            minted(HARNESS_TELL_S + 60, Phase::Pending, "").attention(NOW),
            Attention::Fault(ref n) if n.kind == "harness-stuck"
        ),
        "{:?}",
        minted(HARNESS_TELL_S + 60, Phase::Pending, "").attention(NOW)
    );
    assert_eq!(
        minted(HARNESS_TELL_S + 60, Phase::Pending, "").next_attention_at(NOW - 3_600),
        Some(NOW - HARNESS_TELL_S - 60 + HARNESS_TELL_S),
        "the host wakes at the instant it turns a fault"
    );
    for (why, row) in [
        (
            "announced",
            minted(
                5 * 86_400,
                Phase::Announced {
                    at_s: NOW - 600,
                    asks: 1,
                },
                "",
            ),
        ),
        (
            "a recorded wait",
            minted(5 * 86_400, Phase::Pending, "background"),
        ),
        (
            "no minting instant",
            Row {
                minted_at: 0,
                ..minted(0, Phase::Pending, "")
            },
        ),
    ] {
        assert!(
            !matches!(row.attention(NOW), Attention::Fault(_)),
            "{why}: {:?}",
            row.attention(NOW)
        );
    }
    for wait in [
        "background",
        "not-idle:busy",
        "not-idle:shell",
        "not-idle:waiting",
        "draft",
        "box",
        "login",
        "limited",
        "held",
        "attended",
        "not-foreground",
    ] {
        for waited in [HARNESS_TELL_S + 60, 5 * 86_400] {
            assert!(
                !matches!(
                    at(upgrade::Agent::Claude, wait, waited).attention(NOW),
                    Attention::Fault(_)
                ),
                "{wait} after {waited}s is somebody else's, not aterm's fault"
            );
        }
    }
    for agent in [upgrade::Agent::Claude, upgrade::Agent::Codex] {
        for wait in [
            "left-typed:unsent",
            "left-typed:mixed",
            "left-typed:box",
            "left-typed:no-composer",
        ] {
            let row = at(agent, wait, 5 * 86_400);
            assert!(
                !matches!(row.attention(NOW), Attention::Fault(_)),
                "{wait}: {:?}",
                row.attention(NOW)
            );
            assert_eq!(row.cause_word(NOW), "person", "{wait}");
        }
        assert_eq!(
            wait_class("left-typed:screen-unreadable"),
            WaitClass::TabUnread
        );
        assert!(!person_holds("left-typed:screen-unreadable"));
    }
    assert_eq!(wait_class("home-unreadable"), WaitClass::TabUnread);
}

#[test]
fn the_status_json_says_how_loud_the_owner_is_told_and_why() {
    let text = |row: &Row, key: &str| {
        row.to_json(NOW)
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let waiting = pending(7 * 3_600);
    assert_eq!(text(&waiting, "attention").as_deref(), Some("quiet"));
    assert_eq!(text(&waiting, "attention_kind").as_deref(), Some(""));
    assert_eq!(text(&waiting, "cause").as_deref(), Some("own-work"));
    let draft = Row {
        wait: "draft".to_string(),
        wait_since: NOW - 30 * 3_600,
        ..pending(31 * 3_600)
    };
    assert_eq!(text(&draft, "attention").as_deref(), Some("digest"));
    assert_eq!(
        text(&draft, "attention_kind").as_deref(),
        Some("typed-draft")
    );
    let down = Row {
        phase: Phase::Failed("stale-exit".into()),
        exited_at: NOW - 600,
        ..pending(7 * 3_600)
    };
    assert_eq!(text(&down, "attention").as_deref(), Some("interrupt"));
    assert_eq!(text(&down, "attention_kind").as_deref(), Some("agent-down"));
}

#[test]
fn an_upgrade_that_cannot_run_is_one_owner_sentence() {
    for step in ["busy:state-unwritable", "busy:lock-unopenable"] {
        let need = Need::upgrade_cannot_run(step);
        assert_eq!(need.kind, "upgrade-cannot-run");
        assert_eq!(need.fingerprint, "upgrade-cannot-run/-");
        assert_eq!(need.severity, NeedSeverity::Warn);
        assert!(need.why.chars().count() <= NEED_WHY_MAX);
        assert!(need.then.chars().count() <= NEED_THEN_MAX);
        assert!(!need.then.contains("; ") && !need.then.contains(" or "));
        let placed = format!("{} in tab 12", need.title);
        assert!(placed.split_whitespace().count() <= 7 && placed.chars().count() <= 48);
        assert!(!need.log_line.contains('\n') && need.log_line.contains(step));
    }
    let long = Need::upgrade_cannot_run(&"x".repeat(200));
    assert!(long.log_line.chars().count() < 100, "{}", long.log_line);
    let need = Need::upgrade_records_unlisted();
    assert_eq!(need.kind, "upgrade-records-unlisted");
    assert_eq!(need.fingerprint, "upgrade-records-unlisted/-");
    assert_eq!(need.severity, NeedSeverity::Warn);
    assert!(need.why.chars().count() <= NEED_WHY_MAX);
    assert!(need.then.chars().count() <= NEED_THEN_MAX);
    assert!(!need.then.contains("; ") && !need.then.contains(" or "));
    for text in [&need.why, &need.then, &need.title.to_string()] {
        for n in ["restart", "relaunch", "reopen", "next launch", "reboot"] {
            assert!(!text.to_lowercase().contains(n), "{n} in {text}");
        }
    }
    let placed = format!("{} in tab 12", need.title);
    assert!(placed.split_whitespace().count() <= 7 && placed.chars().count() <= 48);
    assert!(!need.log_line.contains('\n') && need.log_line.contains(need.kind));
    let unread = Need::upgrade_records_unreadable();
    assert_eq!(unread.kind, "upgrade-records-unreadable");
    assert_eq!(unread.fingerprint, "upgrade-records-unreadable/-");
    assert_ne!(unread.kind, need.kind);
    assert_eq!(unread.severity, NeedSeverity::Warn);
    assert!(unread.why.chars().count() <= NEED_WHY_MAX, "{}", unread.why);
    assert!(unread.then.chars().count() <= NEED_THEN_MAX);
    assert!(!unread.then.contains("; ") && !unread.then.contains(" or "));
    for text in [&unread.why, &unread.then, &unread.title.to_string()] {
        for n in ["restart", "relaunch", "reopen", "next launch", "reboot"] {
            assert!(!text.to_lowercase().contains(n), "{n} in {text}");
        }
    }
    let placed = format!("{} in tab 12", unread.title);
    assert!(placed.split_whitespace().count() <= 7 && placed.chars().count() <= 48);
    assert!(!unread.log_line.contains('\n') && unread.log_line.contains(unread.kind));
}

#[test]
fn a_notice_aterm_could_not_type_is_told_as_aterms_own() {
    let now_word: u64 = 1_790_632_250;
    let now = now_word + 20_350;
    let a = Row {
        session: "37dffac7-361e-46be-90c2-31589cb40b5c".to_string(),
        tab: "s-d3346b29dd236432b852".to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.284".to_string(),
        source: "managed".to_string(),
        phase: Phase::Pending,
        behind_since: now_word - 92_270,
        wait: "announce-refused:changed".to_string(),
        wait_since: now_word + 8,
        request: Request::Now,
        request_at: now_word,
        refused: 3,
        refused_at: now_word + 8,
        ..Row::default()
    };
    assert_eq!(a.stall(now).as_deref(), Some("overdue"));
    assert!(a.fence_fails_under_now());
    assert_eq!(
        a.remedy(now),
        Some(Remedy::Waits),
        "the owner's --now is in force: pressing it again moves nothing"
    );
    let unasked = Row {
        request: Request::None,
        request_at: 0,
        ..a.clone()
    };
    assert_eq!(unasked.remedy(now), Some(Remedy::Now));
    assert_eq!(a.badge(now), None, "aterm's own failure marks no tab");
    assert!(
        matches!(a.attention(now), Attention::Fault(ref n) if n.kind == "harness-stuck"),
        "{:?}",
        a.attention(now)
    );
    assert!(a.line(now).ends_with(" refused=3/5h39m"), "{}", a.line(now));

    let young = Row {
        behind_since: now - 2 * 3_600,
        refused_at: now - TELL_S,
        ..a.clone()
    };
    assert!(young.refused_notice(now));
    assert_eq!(young.stall(now).as_deref(), Some("overdue"));
    assert_eq!(young.badge(now), None);
    assert!(matches!(
        young.attention(now),
        Attention::Fault(ref n) if n.kind == "harness-stuck"
    ));
    let switched = Row {
        wait: "announce-refused:yield".to_string(),
        wait_since: now - 60,
        ..young.clone()
    };
    assert!(matches!(switched.attention(now), Attention::Fault(_)));
    for (refused, refused_at) in [(3, now - TELL_S + 60), (0, 0)] {
        let row = Row {
            refused,
            refused_at,
            ..young.clone()
        };
        assert_eq!(row.stall(now), None, "{refused}/{refused_at}");
    }

    let dir = ScratchDir::new("upgrade-status", "refused-word");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let refused_st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: now_s() - 3 * 3_600,
        wait: "announce-refused:changed".to_string(),
        refused: 7,
        refused_at: now_s() - TELL_S - 60,
        ..St::default()
    };
    save(&o, "aaa", &refused_st);
    let row = ask(&o, TAB, Ask::Now, false).expect("the word lands");
    assert_eq!((row.refused, row.refused_at), (0, 0), "{row:?}");
    assert_eq!(row.stall(now_s()), None, "no stall to put back");
    let written = load(&o, "aaa").expect("state");
    assert_eq!((written.refused, written.refused_at), (0, 0));
}

#[test]
fn a_session_asked_again_and_again_is_news() {
    let day = 86_400;
    let holders: Vec<HeldBy> = (0..3)
        .map(|i| HeldBy {
            pid: 4000 + i,
            name: "zsh".into(),
            since: NOW - 600,
        })
        .collect();
    let asked = |asks: u32, behind: u64| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        source: "managed".into(),
        phase: Phase::Announced {
            at_s: NOW - 60,
            asks,
        },
        behind_since: NOW - behind,
        minted_at: NOW - behind,
        noticed_at: NOW - 60,
        wait: "background".into(),
        wait_since: NOW - 3_600,
        own_work_at: NOW - 60,
        held_by: holders.clone(),
        ..Row::default()
    };
    for (asks, behind) in [
        (9, 100 * day),
        (6, 30 * day),
        (upgrade::MAX_ASKS, 26 * 3_600),
    ] {
        match asked(asks, behind).attention(NOW) {
            Attention::Digest(need) => assert_eq!(need.kind, "own-work", "{asks} asks"),
            other => panic!("{asks} asks, {}s behind: {:?}", behind, other.level()),
        }
    }
    let row = Row {
        wait: "box".into(),
        held_by: Vec::new(),
        own_work_at: 0,
        ..asked(9, 100 * day)
    };
    assert!(
        matches!(row.attention(NOW), Attention::Fault(need) if need.kind == "no-progress"),
        "{:?}",
        row.attention(NOW).level()
    );
    assert_eq!(
        asked(upgrade::MAX_ASKS - 1, 30 * day).attention(NOW),
        Attention::Quiet
    );
    assert_eq!(
        asked(9, 20 * 3_600).attention(NOW),
        Attention::Quiet,
        "not yet a day"
    );
    let boxed = Row {
        wait: "box".into(),
        held_by: Vec::new(),
        own_work_at: 0,
        ..asked(9, 100 * day)
    };
    assert!(
        !matches!(boxed.attention(NOW), Attention::Digest(_)),
        "a box is not the agent's work: {:?}",
        boxed.attention(NOW).level()
    );
}

#[test]
fn a_relaunched_agent_that_reaches_no_prompt_is_a_log_line_then_an_interrupt() {
    let relaunched = |age: u64| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Relaunched { at_s: NOW - age },
        behind_since: NOW - age - 600,
        exited_at: NOW - age - 5,
        ..Row::default()
    };
    let young = relaunched(RELAUNCH_STUCK_S - 1);
    assert_eq!(young.attention(NOW), Attention::Quiet);
    assert!(young.badge(NOW).is_none());
    let stuck = relaunched(RELAUNCH_STUCK_S);
    assert!(matches!(stuck.attention(NOW), Attention::Fault(n) if n.kind == "relaunch-stuck"));
    assert!(
        stuck.interrupt(NOW).is_none() && stuck.badge(NOW).is_none(),
        "a log line first"
    );
    for age in [RELAUNCH_INTERRUPT_S, 3_600, 2 * 86_400] {
        let row = relaunched(age);
        let need = row
            .interrupt(NOW)
            .unwrap_or_else(|| panic!("{age} s: {:?}", row.attention(NOW).level()));
        assert_eq!(need.kind, "relaunch-stuck");
        assert!(
            row.stall(NOW).is_some_and(|s| s != "overdue"),
            "{age} s: {:?}",
            row.stall(NOW)
        );
        assert!(need.command.is_none());
        assert_eq!(need.fingerprint, format!("relaunch-stuck/{TAB}"));
        assert!(
            row.badge(NOW)
                .is_some_and(|b| b.len() <= 200 && b.ends_with(&need.then))
        );
    }
    let held = Row {
        request: Request::Skip("2.1.284".into()),
        ..relaunched(3_600)
    };
    assert_eq!(held.attention(NOW), Attention::Quiet);
}

#[test]
fn looks_at_the_same_work_do_not_change_the_row() {
    let t0 = 1_790_000_000;
    let mut st = St {
        phase: Phase::Announced { at_s: t0, asks: 1 },
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        tab: TAB.into(),
        salt: t0,
        pending_since: t0,
        ..St::default()
    };
    let held = |now: u64| {
        vec![upgrade::Held {
            pid: 4000,
            name: "zsh".into(),
            age_s: now - t0,
            command: String::new(),
        }]
    };
    let mut rows: Vec<Row> = Vec::new();
    for minute in 0..30 {
        let now = t0 + 60 * minute;
        st.note_step("wait:background", "busy", now);
        st.note_held("wait:background", &held(now), now);
        rows.push(Row::of("s", &st, now));
    }
    let changes = rows.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!(changes <= 2, "{changes} changes over 30 looks");
    assert!(changes >= 1 || rows[0] == rows[29]);
    let later = t0 + 60 * 30 + super::super::OWN_WORK_RESTAMP_S;
    st.note_step("wait:background", "busy", later);
    st.note_held("wait:background", &held(later), later);
    assert_eq!(st.own_work_at, later);
}

#[test]
fn a_refusal_a_person_or_a_peer_caused_is_not_blamed_on_aterm() {
    let refused = |why: &str, waited: u64| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Pending,
        behind_since: NOW - waited - 3_600,
        minted_at: NOW - waited - 3_600,
        wait: format!("announce-refused:{why}"),
        wait_since: NOW - waited,
        refused: 5,
        refused_at: NOW - waited,
        ..Row::default()
    };
    for why in ["held", "draft", "box"] {
        let row = refused(why, 2 * 3_600);
        assert_eq!(row.attention(NOW), Attention::Quiet, "{why} for 2 h");
        assert!(row.interrupt(NOW).is_none());
    }
    for why in ["held", "box"] {
        let row = refused(why, 2 * 86_400);
        assert!(
            matches!(row.attention(NOW), Attention::Fault(n) if n.kind == "no-progress"),
            "{why} for 2 days: {:?}",
            row.attention(NOW).level()
        );
    }
    assert!(
        matches!(refused("draft", 2 * 86_400).attention(NOW), Attention::Digest(n) if n.kind == "typed-draft")
    );
    let own = refused("changed", 2 * 3_600);
    assert!(
        matches!(own.attention(NOW), Attention::Fault(n) if n.kind == "harness-stuck"),
        "{:?}",
        own.attention(NOW).level()
    );
}

#[test]
fn the_manual_resume_keeps_its_flags_under_every_line_the_harness_builds() {
    let session = "07422bc3-1be9-4875-9672-2eaf7b28aa21";
    let exe = std::path::Path::new("/Users//x/Library/Application Support/aterm/pkg/agents/claude");
    let survey = [("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1")];
    let heal = std::path::Path::new("/Users//x/.aterm/shell.d/00-atpkg.zsh");
    let mut built = 0;
    for flags in [
        &["--dangerously-skip-permissions"][..],
        &[
            "--dangerously-skip-permissions",
            "--model",
            "claude-opus-5-5[1m]",
        ],
        &[
            "--permission-mode",
            "bypassPermissions",
            "--add-dir",
            "/Users//me/My Project",
        ],
        &["--append-system-prompt", "it's"],
    ] {
        let launch: Vec<String> = std::iter::once("claude")
            .chain(flags.iter().copied())
            .map(ToString::to_string)
            .collect();
        let told = crate::harness::upgrade_drive::resume_command_kept(None, &launch, session)
            .expect("a line");
        assert_ne!(told, format!("claude --resume {session}"), "{flags:?}");
        let args: Vec<String> = flags
            .iter()
            .copied()
            .chain(["--resume", session])
            .map(ToString::to_string)
            .collect();
        for dialect in [
            upgrade::Dialect::Zsh,
            upgrade::Dialect::Bash,
            upgrade::Dialect::Fish,
        ] {
            let want =
                if dialect == upgrade::Dialect::Fish && flags.iter().any(|f| f.contains('\'')) {
                    format!("claude --resume {session}")
                } else {
                    told.clone()
                };
            for env in [&survey[..], &[]] {
                for cd in [None, Some("/Users//x/work tree")] {
                    for heal in [None, Some(heal)] {
                        let Ok(line) =
                            upgrade::relaunch_line_env(dialect, heal, cd, env, exe, &args)
                        else {
                            assert!(
                                dialect == upgrade::Dialect::Fish && cd.is_some(),
                                "only fish's cd is refused"
                            );
                            continue;
                        };
                        built += 1;
                        let row = Row {
                            session: session.to_string(),
                            tab: TAB.into(),
                            from: "2.1.283".into(),
                            to: "2.1.284".into(),
                            phase: Phase::Failed("stale-exit".to_string()),
                            exited_at: NOW - 600,
                            line: line.clone(),
                            ..Row::default()
                        };
                        let need = row.interrupt(NOW).expect("agent-down");
                        assert_eq!(need.command.as_deref(), Some(&*want), "{line}");
                        let mark = row.badge(NOW).expect("marked");
                        assert!(mark.len() <= 200 && mark.ends_with(&want), "{mark}");
                    }
                }
            }
        }
    }
    assert!(built >= 4 * 18, "the matrix built {built} lines");
}

#[test]
fn a_resume_by_hand_says_which_login_it_runs_under() {
    use crate::harness::accounts::LOGIN_ENV;
    use crate::harness::resume::SHELL_LOGIN_CLAUSE;
    let session = "07422bc3-1be9-4875-9672-2eaf7b28aa21";
    let dir = "/tmp/aterm state/logins/owner's work-2";
    let exe = std::path::Path::new("/tmp/aterm state/pkg/agents/claude");
    let args: Vec<String> = ["--dangerously-skip-permissions", "--resume", session]
        .iter()
        .map(ToString::to_string)
        .collect();
    let survey = ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1");
    let heal = std::path::Path::new("/tmp/aterm state/shell.d/00-atpkg.zsh");
    let want = format!("claude --dangerously-skip-permissions --resume {session}");
    let down = |why: &str, line: &str| Row {
        session: session.to_string(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Failed(why.to_string()),
        exited_at: NOW - 600,
        line: line.to_string(),
        ..Row::default()
    };
    let mut built = 0;
    for dialect in [upgrade::Dialect::Zsh, upgrade::Dialect::Bash] {
        for login in [Some(dir), Some(""), None] {
            for cd in [None, Some("/tmp/work tree")] {
                for heal in [None, Some(heal)] {
                    let env: Vec<(&str, &str)> = login
                        .map(|value| (LOGIN_ENV, value))
                        .into_iter()
                        .chain([survey])
                        .collect();
                    let line = upgrade::relaunch_line_env(dialect, heal, cd, &env, exe, &args)
                        .expect("a line");
                    built += 1;
                    assert_eq!(resume_login(&line).as_deref(), login, "{dialect:?}: {line}");
                    for why in ["stale-exit", "shell-gone"] {
                        let need = down(why, &line).interrupt(NOW).expect("agent-down");
                        assert_eq!(need.command.as_deref(), Some(&*want), "{line}");
                        assert_eq!(
                            need.then.ends_with(SHELL_LOGIN_CLAUSE),
                            login.is_some(),
                            "{why}: {} <- {line}",
                            need.then
                        );
                        assert!(need.then.chars().count() <= NEED_THEN_MAX, "{}", need.then);
                    }
                }
            }
        }
    }
    assert_eq!(built, 24);
    let moved = format!(" {LOGIN_ENV}='' '/opt/claude' '--resume' '{session}'");
    let plain = format!(" '/opt/claude' '--resume' '{session}'");
    for why in ["stale-exit", "shell-gone"] {
        assert_eq!(
            down(why, &moved).interrupt(NOW).expect("agent-down").then,
            format!(
                "{}{SHELL_LOGIN_CLAUSE}",
                down(why, &plain).interrupt(NOW).expect("agent-down").then
            )
        );
    }
    for line in [
        format!(" '/opt/claude' '--append-system-prompt' '{LOGIN_ENV}=/x' '--resume' '{session}'"),
        format!(" CLAUDE_CONFIG_DIR='/x' '/opt/claude' '--resume' '{session}'"),
        format!(" {LOGIN_ENV}=/x true; '/opt/claude' '--resume' '{session}'"),
        String::new(),
    ] {
        assert_eq!(resume_login(&line), None, "{line}");
    }
    let codex = Row {
        agent: upgrade::Agent::Codex,
        ..down("stale-exit", &moved)
    };
    assert!(
        !codex
            .interrupt(NOW)
            .expect("agent-down")
            .then
            .ends_with(SHELL_LOGIN_CLAUSE)
    );
}

#[test]
fn the_interrupts_action_follows_its_reason_and_keeps_the_launchs_flags() {
    let session = "07422bc3-1be9-4875-9672-2eaf7b28aa21";
    let line = ". '/Users//x/.aterm/shell.d/00-atpkg.zsh'; rehash; '/Users//x/Library/Application Support/aterm/pkg/agents/claude' '--dangerously-skip-permissions' '--resume' '07422bc3-1be9-4875-9672-2eaf7b28aa21'";
    let down = |why: &str, line: &str| Row {
        session: session.to_string(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Failed(why.to_string()),
        exited_at: NOW - 600,
        line: line.to_string(),
        ..Row::default()
    };
    let need = |row: &Row| row.interrupt(NOW).expect("agent-down");
    let n = need(&down("stale-exit", line));
    let in_its_tab = n.then.clone();
    assert_eq!(
        n.command.as_deref(),
        Some(&*format!(
            "claude --dangerously-skip-permissions --resume {session}"
        ))
    );
    let new_tab = need(&down("shell-gone", line)).then;
    assert_ne!(new_tab, in_its_tab);
    for why in ["no-resume", "relaunch-refused", "exited-before-continuing"] {
        assert_eq!(need(&down(why, line)).then, in_its_tab, "{why}");
    }
    let launched = |flags: &str, login: bool| {
        let export = if login {
            format!(
                "{}='/Users//x/.aterm/accounts/work' ",
                crate::harness::accounts::LOGIN_ENV
            )
        } else {
            String::new()
        };
        format!(". '/x/rc.zsh'; rehash; {export}'/x/claude' {flags} '--resume' '{session}'")
    };
    let skip = format!("claude --dangerously-skip-permissions --resume {session}");
    let plan = format!(
        "claude --allow-dangerously-skip-permissions --permission-mode plan --resume {session}"
    );
    let clause = crate::harness::resume::SHELL_LOGIN_CLAUSE;
    for (flags, login, why, clause_kept, want) in [
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' '/x/src'",
            true,
            "stale-exit",
            true,
            skip.as_str(),
        ),
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' '/x/src'",
            true,
            "shell-gone",
            true,
            skip.as_str(),
        ),
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' \
             '/Users//x/src'",
            false,
            "stale-exit",
            false,
            skip.as_str(),
        ),
        (
            "'--allow-dangerously-skip-permissions' '--permission-mode' 'plan' \
             '--append-system-prompt' 'You orchestrate this repo'",
            true,
            "shell-gone",
            false,
            plan.as_str(),
        ),
    ] {
        let row = down(why, &launched(flags, login));
        let n = need(&row);
        assert_eq!(
            (n.then.ends_with(clause), n.command.as_deref()),
            (clause_kept, Some(want)),
            "{why}: {flags}"
        );
        let mark = row.badge(NOW).expect("marked");
        assert!(mark.len() <= 200 && mark.ends_with(want), "{why}: {mark}");
    }
    let codex = |why: &str| Row {
        agent: upgrade::Agent::Codex,
        ..down(why, "")
    };
    assert_eq!(
        need(&codex("no-resume-hint")).command.as_deref(),
        Some("codex resume")
    );
    let gone = need(&codex("no-rollout"));
    assert_eq!(gone.command, None);
    let long = format!(
        ". '/x/rc.zsh'; rehash; '/x/claude' '--model' '{}' '--resume' 'id'",
        "m".repeat(90)
    );
    for odd in [
        "",
        long.as_str(),
        ". '/x/rc.zsh'; rehash; '/x/other' '--flag' '--resume' 'id'",
        ". '/x/rc.zsh'; rehash; '/x/claude' '--flag'",
    ] {
        assert_eq!(
            need(&down("stale-exit", odd)).command.as_deref(),
            Some(&*format!("claude --resume {session}")),
            "{odd:?}"
        );
    }
}

#[test]
fn the_own_work_line_names_the_goal_the_programs_and_the_break() {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Then, Why};
    let day = 86_400;
    let holders = |n: u32| -> Vec<HeldBy> {
        (0..n)
            .map(|i| HeldBy {
                pid: 4000 + i,
                name: "zsh".into(),
                since: NOW - 600,
            })
            .collect()
    };
    let round = |wait: &str, held: u32, goal: bool| Row {
        session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 2,
        behind_since: NOW - 2 * day,
        minted_at: NOW - 2 * day,
        wait: wait.into(),
        wait_since: NOW - 3_600,
        own_work_at: NOW - 60,
        held_by: holders(held),
        goal,
        ..Row::default()
    };
    let words = |row: &Row| match row.attention(NOW) {
        Attention::Digest(n) => {
            assert_eq!(n.kind, "own-work");
            assert!(
                n.why.chars().count() <= NEED_WHY_MAX && n.then.chars().count() <= NEED_THEN_MAX
            );
            let entry = Entry::new(
                Family::Upgrade,
                "",
                n.kind,
                "Claude in tab 3 is behind",
                Why::OnlyYou(Reason::new(&n.why)),
                Then::step(&n.then),
            );
            if let Err(fault) = entry.message() {
                panic!("the door refuses {:?} / {:?}: {fault:?}", n.why, n.then);
            }
            (n.why, n.then)
        }
        other => panic!("{:?}", other.level()),
    };
    let arms = [
        ("failed", 16, true),
        ("failed", 1, true),
        ("failed", 0, true),
        ("failed", 16, false),
        ("failed", 1, false),
        ("background", 0, false),
        ("background-terminal", 0, false),
        ("release:background", 0, false),
        ("failed", 0, false),
    ];
    let said: Vec<(String, String)> = arms
        .iter()
        .map(|(wait, held, goal)| words(&round(wait, *held, *goal)))
        .collect();
    for ((wait, held, goal), (why, then)) in arms.iter().zip(&said) {
        let at = format!("{wait} held={held} goal={goal}: {why} / {then}");
        assert_eq!(why.contains("/goal"), *goal, "{at}");
        assert_eq!(then.contains("/goal"), *goal, "{at}");
        let breaking = wait.contains("background") && *held == 0 && !*goal;
        assert_eq!(why.contains("background"), breaking, "{at}");
    }
    let mut whys: Vec<&str> = [0, 2, 3, 5, 8]
        .iter()
        .map(|i| said[*i].0.as_str())
        .collect();
    whys.sort_unstable();
    whys.dedup();
    assert_eq!(whys.len(), 5, "{said:?}");
}

#[test]
fn the_own_work_line_does_not_move_with_the_holder_count() {
    let look = |held: u32, goal: bool| {
        let row = Row {
            session: "07422bc3-1be9-4875-9672-2eaf7b28aa21".into(),
            tab: TAB.into(),
            from: "2.1.283".into(),
            to: "2.1.284".into(),
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            streak_why: upgrade::GAVE_UP.to_string(),
            stop_streak: 2,
            behind_since: NOW - 2 * 86_400,
            minted_at: NOW - 2 * 86_400,
            wait: "failed".into(),
            wait_since: NOW - 3_600,
            own_work_at: NOW - 60,
            held_by: (0..held)
                .map(|i| HeldBy {
                    pid: 4000 + i,
                    name: "zsh".into(),
                    since: NOW - 600,
                })
                .collect(),
            goal,
            ..Row::default()
        };
        let Attention::Digest(need) = row.attention(NOW) else {
            panic!("own work at {held} programs");
        };
        let more = row.more_lines(&need, NOW);
        (
            (
                need.fingerprint,
                need.title,
                need.severity,
                need.why,
                need.then,
            ),
            more,
        )
    };
    for goal in [false, true] {
        let (one, one_more) = look(1, goal);
        let (two, _) = look(2, goal);
        let (many, many_more) = look(16, goal);
        assert_eq!(one, two, "goal={goal}");
        assert_eq!(one, many, "goal={goal}");
        assert_ne!(one_more, many_more, "goal={goal}: the details say how many");
    }
}

#[test]
fn the_ask_count_survives_a_retarget_and_the_state_file() {
    let dir = ScratchDir::new("upgrade-status", "ask-count");
    let o = opts(&dir);
    let day = 86_400;
    let t0 = NOW - 9 * day;
    let running = Version::parse("2.1.283").expect("running");
    let build = |v: &str| upgrade::Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse(v).expect("target"),
        source: upgrade::Source::Managed,
    };
    let who = super::super::Proc {
        start: "Thu Sep 24 08:00:00 2026",
        started_s: t0 - 600,
    };
    let mut st = St::for_target(None, &running, &build("2.1.284"), None, who, t0);
    st.tab = TAB.to_string();
    st.announced("m1".to_string(), t0 + 60, 1);
    st.announced("m2".to_string(), t0 + 1_860, 2);
    let (mut st, how) = St::retarget(
        Some(st),
        &running,
        &build("2.1.285"),
        Some("claude-opus-4-1"),
        who,
        NOW - 3 * day,
    );
    assert_eq!(how, super::super::Retarget::Anew, "the round starts over");
    assert_eq!(st.phase, Phase::Pending);
    for ask in 1..=4_u32 {
        let at = NOW - 4 * 3_600 + u64::from(ask) * 1_800;
        st.announced(format!("n{ask}"), at, ask);
    }
    st.give_up(NOW - 600);
    st.own_work_at = NOW - 60;
    super::super::save_at(&o, "07422bc3-1be9-4875-9672-2eaf7b28aa21", &st, NOW);
    let (rows, whole) = read_rows(&o, NOW);
    assert!(whole);
    let [row] = rows.as_slice() else {
        panic!("one record: {rows:?}");
    };
    let Attention::Digest(need) = row.attention(NOW) else {
        panic!("{:?}", row.attention(NOW).level());
    };
    assert_eq!(need.kind, "own-work");
    let more = row.more_lines(&need, NOW);
    assert!(more[0].contains(" 6 "), "{more:?}");
    let other = super::super::Proc {
        start: "Fri Oct  2 09:00:00 2026",
        started_s: NOW - 60,
    };
    let anew = St::for_target(Some(st), &running, &build("2.1.285"), None, other, NOW);
    assert_eq!(anew.notices, 0, "{anew:?}");
}

#[path = "upgrade_progress_tests.rs"]
mod progress;

#[test]
fn a_rearmed_rounds_first_looks_are_still_its_rest() {
    let rearmed = |wait: &str, since: u64| Row {
        last_stop: upgrade::GAVE_UP.to_string(),
        wait_since: since,
        ..pending_with(wait)
    };
    for (wait, since) in [("", 0), ("attended", NOW - 30), ("draft", NOW - 60)] {
        let row = rearmed(wait, since);
        assert_eq!(row.stall(NOW).as_deref(), Some("overdue"), "{wait:?}");
        assert!(row.asks_on_its_own(NOW), "{wait:?}: its rest, a record");
        assert_eq!(row.badge(NOW), None, "{wait:?}: no mark");
    }
    for wait in ["", "attended", "draft"] {
        assert!(
            !pending_with(wait).asks_on_its_own(NOW),
            "{wait:?}: a first round is a row"
        );
    }
    let stood = rearmed("draft", NOW - upgrade::REASK_S);
    assert!(!stood.asks_on_its_own(NOW), "a draft that stood: a row");
    assert_eq!(stood.badge(NOW), None, "no mark for a wait");
    let queued = rearmed("queued", NOW - 60);
    assert!(
        !queued.asks_on_its_own(NOW),
        "`Upgrade now` moves it: a row"
    );
}

#[test]
fn what_time_alone_changes_is_a_changed_row_at_a_named_instant() {
    let read = |mut row: Row, now: u64| {
        row.read_at(now);
        row
    };
    let overdue = read(pending(7 * 3_600), NOW);
    assert!(!overdue.on_its_own);
    assert_eq!(overdue.behind_said.as_deref(), Some("7 h"));
    assert_eq!(
        next_change(std::slice::from_ref(&overdue), NOW),
        Some(NOW + 3_600)
    );
    assert_eq!(read(pending(7 * 3_600), NOW), overdue, "the same second");
    let later = read(pending(7 * 3_600), NOW + 3_600);
    assert_eq!(later.behind_said.as_deref(), Some("8 h"));
    assert_ne!(later, overdue, "an hour on: sent again");
    let rearmed = |wait: &str| Row {
        last_stop: upgrade::GAVE_UP.to_string(),
        wait_since: NOW - 60,
        ..pending_with(wait)
    };
    let resting = read(rearmed("not-idle:shell"), NOW);
    assert!(resting.on_its_own);
    assert_eq!(resting.behind_said.as_deref(), Some("7 h"));
    assert_eq!(
        next_change(std::slice::from_ref(&resting), NOW),
        Some(NOW + 3_600)
    );
    let resting_later = read(rearmed("not-idle:shell"), NOW + 3_600);
    assert!(resting_later.on_its_own, "still a record an hour on");
    assert_eq!(resting_later.behind_said.as_deref(), Some("8 h"));
    assert_ne!(resting_later, resting, "an hour on: sent again");
    let half = NOW + 1_800;
    assert_eq!(
        next_change(
            std::slice::from_ref(&read(rearmed("not-idle:shell"), half)),
            half
        ),
        Some(NOW + 3_600)
    );
    let past = read(rearmed("not-idle:shell"), NOW - 7 * 3_600 + MOVE_BUDGET_S);
    assert!(!past.on_its_own);
    assert_ne!(past, resting);
    let draft = read(rearmed("draft"), NOW);
    assert!(draft.on_its_own, "a draft just typed: its rest");
    let stands = NOW - 60 + upgrade::REASK_S;
    assert_eq!(next_change(std::slice::from_ref(&draft), NOW), Some(stands));
    let stood = read(rearmed("draft"), stands);
    assert!(!stood.on_its_own, "a hold that stood: a row");
    assert_ne!(stood, draft);
}

#[test]
fn a_stall_whose_words_carry_no_age_never_wakes_for_it() {
    let read = |mut row: Row, now: u64| {
        row.read_at(now);
        row
    };
    let held = Row {
        wait: "terminal:vim".to_string(),
        ..pending(5 * 60)
    };
    let refused = Row {
        phase: Phase::Failed("not-a-shell-job".to_string()),
        ..pending(5 * 60)
    };
    for (what, row) in [("held-back", held), ("refused", refused)] {
        let first = read(row.clone(), NOW);
        let stall = first.stalled.clone().expect("a stall");
        assert_ne!(stall, "overdue", "{what}");
        assert!(!first.on_its_own, "{what}: a row");
        assert_eq!(first.behind_said, None, "{what}: its words carry no age");
        assert_eq!(first.age_said(NOW), None, "{what}");
        let wake = next_change(std::slice::from_ref(&first), NOW);
        assert!(
            wake.is_none_or(|t| t >= NOW + 3_600),
            "{what}: no minute wake, got {wake:?}"
        );
        assert_eq!(
            read(row, NOW + 60),
            first,
            "{what}: a minute on, nothing to send"
        );
    }
}

#[test]
fn an_announced_round_under_the_quiet_flow_reads_overdue_at_six_hours() {
    let announced = |quiet: bool| Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 2,
        },
        quiet,
        ..pending(STALLED_AFTER_S + 3_600)
    };
    let quiet = announced(true);
    assert_eq!(quiet.stall(NOW).as_deref(), Some("overdue"));
    assert!(!quiet.asks_on_its_own(NOW), "nothing asks it again");
    assert_eq!(quiet.badge(NOW), None, "a record, never a mark");
    assert!(matches!(quiet.attention(NOW), Attention::Quiet));
    let mut sent = quiet.clone();
    sent.read_at(NOW);
    assert!(!sent.on_its_own, "sent as a row");
    let asked = announced(false);
    assert!(asked.asks_on_its_own(NOW), "the ask flow asks again");
    assert_eq!(asked.badge(NOW), None);
    for quiet in [true, false] {
        let limited = Row {
            wait: "limited".into(),
            ..announced(quiet)
        };
        assert!(limited.asks_on_its_own(NOW), "a limit: {quiet}");
        assert_eq!(limited.badge(NOW), None, "{quiet}");
    }
    let dir = ScratchDir::new("upgrade-status", "quiet-flow");
    let mut o = opts(&dir);
    let st = St {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW);
    assert!(!rows(&o)[0].quiet, "`upgrade_ask = true`");
    o.ask = false;
    assert!(rows(&o)[0].quiet, "the default");
}

#[test]
fn a_lagging_status_under_the_quiet_flow_is_no_comfort_and_now_moves_it() {
    for status in ["busy", "shell"] {
        let row = |quiet: bool, behind_for: u64| Row {
            wait: format!("status-stale:{status}"),
            quiet,
            ..pending(behind_for)
        };
        let quiet = row(true, 3_600);
        assert!(!quiet.waits_for_comfort(), "{status}");
        assert_eq!(
            row(true, STALLED_AFTER_S + 60).remedy(NOW),
            Some(Remedy::Now),
            "{status}"
        );
        assert!(row(false, 3_600).waits_for_comfort(), "{status}");
        assert_eq!(
            row(false, STALLED_AFTER_S + 60).remedy(NOW),
            Some(Remedy::Waits),
            "{status}"
        );
    }
}

#[test]
fn a_row_served_late_is_judged_with_the_awake_clock_as_it_stands() {
    let bed = NOW - 8 * 3_600;
    let slept = Row {
        watch_at: bed + 3_600,
        looked_at: bed - 60,
        awake_since: bed - 86_400,
        ..pending(3_600)
    };
    assert_eq!(slept.stall(NOW).as_deref(), Some("unwatched"), "as read");
    pin_awake_since(NOW - 60);
    let mut served = slept.clone();
    served.reread(NOW);
    assert_eq!(served.awake_since, NOW - 60);
    assert_eq!(served.stall(NOW), None, "inside the wake grace");
    assert_eq!(served.stalled, None, "and read so");
    pin_awake_since(bed - 86_400);
    let mut hosted = Row {
        awake_since: NOW - 30,
        ..slept.clone()
    };
    hosted.reread(NOW);
    assert_eq!(hosted.awake_since, NOW - 30, "never lowered");
    assert_eq!(hosted.stall(NOW), None);
    pin_awake_since(0);
}

#[test]
fn a_quiet_give_up_its_own_work_held_is_a_wait_the_owners_now_does_not_move() {
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 3_600,
        failed_at: NOW - 60,
        work_seen: true,
        ..St::default()
    };
    let quiet = |st: &St| Row {
        quiet: true,
        ..Row::of("c", st, NOW)
    };
    let row = quiet(&gave_up);
    assert!(row.work_seen);
    assert!(row.rests_on_own_work());
    assert_eq!(row.remedy(NOW), Some(Remedy::Waits));
    let running = quiet(&St {
        work_seen: false,
        held_by: vec![HeldBy {
            pid: 4242,
            name: "zsh".to_string(),
            since: NOW - 7_200,
        }],
        ..gave_up.clone()
    });
    assert!(running.rests_on_own_work());
    assert_eq!(running.remedy(NOW), Some(Remedy::Waits));
    let asked = Row {
        quiet: false,
        ..row.clone()
    };
    assert!(!asked.rests_on_own_work());
    assert_eq!(asked.remedy(NOW), Some(Remedy::AskAgain));
    let lagged = quiet(&St {
        work_seen: false,
        ..gave_up.clone()
    });
    assert!(!lagged.rests_on_own_work());
    assert_eq!(lagged.remedy(NOW), Some(Remedy::AskAgain));
    for (why, st) in [
        (
            "a status that lags",
            St {
                wait: "status-stale:shell".to_string(),
                ..gave_up.clone()
            },
        ),
        (
            "a Codex give-up",
            St {
                agent: upgrade::Agent::Codex,
                ..gave_up.clone()
            },
        ),
    ] {
        let row = quiet(&st);
        assert!(row.work_seen && !row.rests_on_own_work(), "{why}");
        assert_eq!(row.remedy(NOW), Some(Remedy::AskAgain), "{why}");
    }
    let announced = quiet(&St {
        phase: Phase::Announced {
            at_s: NOW - 60,
            asks: 1,
        },
        ..gave_up.clone()
    });
    assert!(!announced.work_seen);
    assert!(!announced.rests_on_own_work());
    let skipped = quiet(&St {
        request: Request::Skip("2.1.282".to_string()),
        request_tab: TAB.to_string(),
        request_at: NOW - 10,
        ..gave_up
    });
    assert_eq!(skipped.remedy(NOW), None);
}

#[test]
fn a_token_word_reads_as_the_cut_reads_it() {
    let long = "a".repeat(64);
    let longer = "b".repeat(65);
    let longer_capped = "b".repeat(64);
    for (s, want) in [
        ("claude", "claude"),
        ("wait:own-work(3)", "wait:own-work(3)"),
        ("0.158.0_x", "0.158.0_x"),
        (long.as_str(), long.as_str()),
        (longer.as_str(), longer_capped.as_str()),
        ("", "-"),
        ("two words", "twowords"),
        ("\u{e9}", "-"),
        ("tab\t", "tab"),
        ("!!", "-"),
    ] {
        assert_eq!(word(s), want, "{s:?}");
    }
}

#[test]
fn a_finished_moves_row_carries_its_relaunch_instant() {
    let now = now_s();
    let st = St {
        phase: Phase::Done,
        from: "2.1.286".to_string(),
        to: "2.1.287".to_string(),
        tab: TAB.to_string(),
        done_at: now - 5,
        relaunched_at: now - 7 * 86_400,
        ..St::default()
    };
    assert_eq!(Row::of("aaa", &st, now).moved_at, now - 7 * 86_400);
    let older = St {
        relaunched_at: 0,
        ..st
    };
    assert_eq!(Row::of("aaa", &older, now).moved_at, 0);
}

#[test]
fn a_build_that_fails_to_start_is_one_fault_per_build() {
    use crate::harness::upgrade_drive::BUILD_FAILS;
    let held = |tab: &str, to: &str| Row {
        tab: tab.to_string(),
        to: to.to_string(),
        wait: BUILD_FAILS.to_string(),
        ..pending(3_600)
    };
    let fault = |row: &Row| match row.attention(NOW) {
        Attention::Fault(need) => need,
        other => panic!("a fault, not {other:?}"),
    };
    let here = fault(&held(TAB, "2.1.999"));
    let there = fault(&held("s-0th3r0th3r0th3r0th3r", "2.1.999"));
    assert_eq!(here.kind, "build-fails");
    assert_eq!(here.fingerprint, there.fingerprint, "one per build");
    assert_ne!(here.fingerprint, fault(&held(TAB, "2.1.998")).fingerprint);
    assert!(here.why.contains("2.1.999"), "{}", here.why);
    let row = held(TAB, "2.1.999");
    assert_eq!(row.interrupt(NOW), None);
    assert_eq!(row.cause_word(NOW), "build-fails");
    let stopped = Row {
        phase: Phase::Failed(BUILD_FAILS.to_string()),
        wait: "failed".to_string(),
        streak_why: BUILD_FAILS.to_string(),
        stop_streak: 1,
        ..held("s-0th3r0th3r0th3r0th3r", "2.1.999")
    };
    assert_eq!(fault(&stopped).fingerprint, here.fingerprint);
    assert_eq!(stopped.interrupt(NOW), None);
}

#[test]
fn two_moves_of_one_tab_both_reach_the_window_and_its_row_is_the_newer() {
    let now = now_s();
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    for (done, failed) in [("aaa", "bbb"), ("bbb", "aaa")] {
        let dir = ScratchDir::new("upgrade-status", &format!("tab-rows-{done}"));
        let o = opts(&dir);
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        let move_to = |from: &str, to: &str, minted: u64| St {
            from: from.to_string(),
            to: to.to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            salt: now - minted,
            pending_since: now - minted,
            ..St::default()
        };
        save(
            &o,
            done,
            &St {
                phase: Phase::Done,
                done_at: now - 240,
                ..move_to("2.1.281", "2.1.282", 3_600)
            },
        );
        save(
            &o,
            failed,
            &St {
                phase: Phase::Failed("stale-exit".to_string()),
                exited_at: now - 100,
                failed_at: now - 100,
                ..move_to("2.1.282", "2.1.283", 200)
            },
        );
        let mut summary: Vec<Row> = Vec::new();
        let _ = View::default().refresh_with(&o, &tabs, now, &|_| Some(Vec::new()), &mut |r| {
            summary = r.to_vec()
        });
        let all: Vec<(&str, &str)> = summary
            .iter()
            .map(|r| (r.session.as_str(), r.tab.as_str()))
            .collect();
        assert_eq!(all, [("aaa", TAB), ("bbb", TAB)], "both, by session id");
        let tab: Vec<(String, String)> = tab_rows(&summary)
            .into_iter()
            .map(|r| (r.session, r.to))
            .collect();
        assert_eq!(
            tab,
            [(failed.to_string(), "2.1.283".to_string())],
            "{done} finished"
        );
    }
}

#[test]
fn a_tabs_row_is_its_agent_in_front_then_its_newest_move() {
    let at = |session: &str, phase: Phase, minted: u64| Row {
        session: session.to_string(),
        phase,
        minted_at: NOW - minted,
        ..pending(3_600)
    };
    let done = |session: &str, minted: u64| Row {
        done_at: NOW - 60,
        ..at(session, Phase::Done, minted)
    };
    let aside = Row {
        aside: NOT_FOREGROUND.to_string(),
        ..at("aside", Phase::Pending, 60)
    };
    let down = |session: &str, minted: u64| Row {
        exited_at: NOW - 100,
        ..at(session, Phase::Failed("stale-exit".to_string()), minted)
    };
    let pick =
        |rows: &[Row]| -> Vec<String> { tab_rows(rows).into_iter().map(|r| r.session).collect() };
    for (winner, loser, why) in [
        (
            done("done", 3_600),
            aside.clone(),
            "aside yields to a finished move",
        ),
        (
            at("front", Phase::Pending, 3_600),
            aside.clone(),
            "in front beats aside minted later",
        ),
        (
            at("flight", Phase::Relaunched { at_s: NOW - 30 }, 3_600),
            at("front", Phase::Pending, 60),
            "a restart in flight beats a live conversation minted later",
        ),
        (
            at("front", Phase::Pending, 3_600),
            done("done", 60),
            "a live conversation beats a finished move minted later",
        ),
        (down("newer", 200), done("older", 3_600), "the newer record"),
        (
            down("down", 3_600),
            at("front", Phase::Pending, 60),
            "a Claude Code move still down beats a live conversation minted later",
        ),
        (
            at("flight", Phase::Relaunched { at_s: NOW - 30 }, 3_600),
            down("down", 60),
            "a restart in flight beats a move down minted later",
        ),
        (
            at("front", Phase::Pending, 3_600),
            Row {
                agent: upgrade::Agent::Codex,
                ..down("codex", 60)
            },
            "a live conversation beats a Codex move down minted later",
        ),
        (down("newer", 600), down("older", 7_200), "the newer record"),
    ] {
        let name = winner.session.clone();
        assert_eq!(
            pick(&[winner.clone(), loser.clone()]),
            std::slice::from_ref(&name),
            "{why}"
        );
        assert_eq!(pick(&[loser, winner]), [name], "{why}");
    }
    let other = Row {
        tab: "s-other".to_string(),
        ..at("other", Phase::Pending, 60)
    };
    assert_eq!(pick(&[done("done", 60), other.clone()]), ["done", "other"]);
    assert_eq!(pick(&[other.clone(), done("done", 60)]), ["other", "done"]);
    assert_eq!(pick(&[done("done", 60)]), ["done"]);
    let untabbed = Row {
        tab: String::new(),
        ..at("none", Phase::Pending, 60)
    };
    assert_eq!(
        pick(&[
            untabbed,
            done("done", 3_600),
            at("front", Phase::Pending, 60)
        ]),
        ["none", "front"]
    );
}

#[test]
fn a_tabs_mark_is_its_rows_badge() {
    let down = |session: &str, to: &str, minted: u64| Row {
        session: session.to_string(),
        to: to.to_string(),
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: NOW - 300,
        minted_at: NOW - minted,
        wait: String::new(),
        ..pending(7_200)
    };
    let newer = down("aaa", "2.1.283", 600);
    let older = down("zzz", "2.1.282", 7_200);
    let badge = |r: &Row| r.badge(NOW).expect("an agent the move ended is marked");
    assert_ne!(badge(&newer), badge(&older), "the two marks tell apart");
    for rows in [
        [newer.clone(), older.clone()],
        [older.clone(), newer.clone()],
    ] {
        assert_eq!(marks(&rows, NOW).get(TAB), Some(&badge(&newer)));
    }
    let front = Row {
        phase: Phase::Pending,
        exited_at: 0,
        wait: "queued".to_string(),
        ..newer
    };
    assert_eq!(front.badge(NOW), None, "a pending move is not marked");
    for rows in [
        [front.clone(), older.clone()],
        [older.clone(), front.clone()],
    ] {
        assert_eq!(marks(&rows, NOW).get(TAB), Some(&badge(&older)));
        let tab: Vec<String> = tab_rows(&rows).into_iter().map(|r| r.session).collect();
        assert_eq!(tab, std::slice::from_ref(&older.session));
    }
    assert_eq!(
        marks(std::slice::from_ref(&older), NOW).get(TAB),
        Some(&badge(&older))
    );
    let finished = Row {
        phase: Phase::Done,
        done_at: NOW - 60,
        exited_at: 0,
        ..older
    };
    for rows in [
        [front.clone(), finished.clone()],
        [finished.clone(), front.clone()],
    ] {
        assert!(!marks(&rows, NOW).contains_key(TAB));
        let tab: Vec<String> = tab_rows(&rows).into_iter().map(|r| r.session).collect();
        assert_eq!(tab, std::slice::from_ref(&front.session));
    }
}

#[cfg(unix)]
#[test]
fn a_conversation_pending_beside_a_move_down_leaves_the_tab_its_mark() {
    let dir = ScratchDir::new("upgrade-status", "down-beside-pending");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let old = moved_state_as(
        &o,
        upgrade::Agent::Claude,
        Phase::Failed("no-resume".to_string()),
        now_s() - 120,
        fresh,
    );
    save(
        &o,
        NEW,
        &St {
            phase: Phase::Pending,
            from: "2.1.282".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            salt: now_s() - 60,
            pending_since: now_s() - 60,
            ..St::default()
        },
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let agent = |led: bool| {
        move |_: &BTreeSet<String>| -> Option<Vec<Holder>> {
            Some(vec![Holder {
                led,
                ..holder(NEW, "2.1.282", Some(TAB))
            }])
        }
    };
    let meta = || -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
            .unwrap_or_default()
    };
    let clear = || asked.lock().expect("log").clear();
    let unset = format!("@{TAB} meta unset attention owner={ATTENTION_OWNER}");
    let mut view = View::default();
    let mut shown: Vec<Row> = Vec::new();
    let _ = view.refresh_with(&o, &tabs, now_s(), &agent(false), &mut |r| {
        shown = r.to_vec();
    });
    let sessions: Vec<&str> = shown.iter().map(|r| r.session.as_str()).collect();
    assert_eq!(sessions, [old.as_str(), NEW], "both stand");
    let tab = tab_rows(&shown);
    assert_eq!(tab.len(), 1, "{tab:?}");
    assert_eq!(tab[0].session, old, "the move down speaks for the tab");
    assert!(matches!(
        tab[0].attention(now_s()),
        Attention::Interrupt(ref need) if need.kind == "agent-down"
    ));
    let badge = tab[0].badge(now_s()).expect("an interrupt is marked");
    let set = format!("@{TAB} meta set attention owner={ATTENTION_OWNER} {badge}");
    assert!(meta().contains(&set), "{:?}", meta());
    assert!(!meta().contains(&unset), "{:?}", meta());
    clear();
    let _ = view.refresh_with(&o, &tabs, now_s(), &agent(false), &mut |r| {
        shown = r.to_vec();
    });
    assert!(meta().is_empty(), "{:?}", meta());

    clear();
    let _ = view.refresh_with(&o, &tabs, now_s(), &agent(true), &mut |r| {
        shown = r.to_vec();
    });
    let sessions: Vec<&str> = shown.iter().map(|r| r.session.as_str()).collect();
    assert_eq!(sessions, [NEW]);
    assert_eq!(meta(), [unset]);
}

#[test]
fn the_owners_word_lands_on_the_row_the_window_shows() {
    const FRONT: &str = "f0000000-1111-2222-3333-444455556666";
    const ASIDE: &str = "a5000000-1111-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "ask-tab-row");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let behind = |to: &str, minted: u64| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: now - minted,
        pending_since: now - minted,
        ..St::default()
    };
    let both = |aside: bool| {
        move |_: &BTreeSet<String>| -> Option<Vec<Holder>> {
            Some(vec![
                holder(FRONT, "2.1.281", Some(TAB)),
                Holder {
                    aside,
                    ..holder(ASIDE, "2.1.281", Some(TAB))
                },
            ])
        }
    };
    let word = |session: &str, held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        save(&o, FRONT, &behind("2.1.282", 3_600));
        save(&o, ASIDE, &behind("2.1.282", 60));
        let shown = Shown {
            to: "2.1.282",
            session,
            scope: None,
        };
        let shown = (!session.is_empty()).then_some(shown);
        let (row, _) = ask_within(&o, TAB, Ask::Skip, shown, ASK_LOCK_WAIT, held).expect("lands");
        let other = if row.session == FRONT { ASIDE } else { FRONT };
        assert_eq!(load(&o, other).expect("kept").request, Request::None);
        row.session
    };
    assert_eq!(word("", &both(true)), FRONT, "the agent in front");
    assert_eq!(word(FRONT, &both(true)), FRONT);
    assert_eq!(word(ASIDE, &both(true)), ASIDE, "the row the window showed");
    assert_eq!(word("", &both(false)), ASIDE, "both in front: the newest");
    assert_eq!(word(NEW, &both(true)), FRONT, "no record: the tab's rule");

    let dir = ScratchDir::new("upgrade-status", "ask-down-row");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let down = moved_state(
        &o,
        upgrade::Agent::Claude,
        Phase::Failed("stale-exit".to_string()),
        now - 120,
    );
    save(&o, "stale", &behind("2.1.283", 60));
    let back = |_: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(vec![holder(NEW, "2.1.283", Some(TAB))])
    };
    let shown = Shown {
        to: "2.1.283",
        session: &down,
        scope: None,
    };
    let (row, _) = ask_within(&o, TAB, Ask::Skip, Some(shown), ASK_LOCK_WAIT, &back)
        .expect("the move's record takes it");
    assert_eq!(row.session, down);
    assert_eq!(
        load(&o, &down).expect("written").request,
        Request::Skip("2.1.283".to_string())
    );
    assert_eq!(load(&o, "stale").expect("kept").request, Request::None);
}

#[test]
fn a_resume_by_hand_carries_the_claude_home_its_relaunch_line_carried() {
    use crate::harness::accounts::{HOME_ENV, LOGIN_ENV};
    let session = "07422bc3-1be9-4875-9672-2eaf7b28aa21";
    let exe = std::path::Path::new("/opt/claude");
    let args: Vec<String> = ["--dangerously-skip-permissions", "--resume", session]
        .iter()
        .map(ToString::to_string)
        .collect();
    let survey = ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1");
    let down = |line: &str| Row {
        session: session.to_string(),
        tab: TAB.into(),
        from: "2.1.283".into(),
        to: "2.1.284".into(),
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: NOW - 600,
        line: line.to_string(),
        ..Row::default()
    };
    let told = format!("claude --dangerously-skip-permissions --resume {session}");
    for cd in [None, Some("/tmp/work tree")] {
        let env = [
            (HOME_ENV, "/tmp/abs home/.claude"),
            ("HOME", "/u/other"),
            (LOGIN_ENV, "/l"),
            survey,
        ];
        let line = upgrade::relaunch_line_env(upgrade::Dialect::Zsh, None, cd, &env, exe, &args)
            .expect("a line");
        let need = down(&line).interrupt(NOW).expect("agent-down");
        assert_eq!(
            need.command,
            Some(format!(
                "CLAUDE_CONFIG_DIR='/tmp/abs home/.claude' HOME=/u/other {told}"
            )),
            "{line}"
        );
        let home_only = upgrade::relaunch_line_env(
            upgrade::Dialect::Zsh,
            None,
            cd,
            &[("HOME", "/u/other"), survey],
            exe,
            &args,
        )
        .expect("a line");
        assert_eq!(
            down(&home_only).interrupt(NOW).expect("agent-down").command,
            Some(format!("HOME=/u/other {told}")),
            "{home_only}"
        );
    }
    let long = format!("/u/{}", "h".repeat(240));
    let line = upgrade::relaunch_line_env(
        upgrade::Dialect::Zsh,
        None,
        None,
        &[("HOME", &long)],
        exe,
        &args,
    )
    .expect("a line");
    let need = down(&line).interrupt(NOW).expect("agent-down");
    assert_eq!(need.command, None, "{line}");
    assert!(!need.then.contains("this command"), "{}", need.then);
    let plain =
        upgrade::relaunch_line_env(upgrade::Dialect::Zsh, None, None, &[survey], exe, &args)
            .expect("a line");
    assert_eq!(
        down(&plain).interrupt(NOW).expect("agent-down").command,
        Some(told)
    );
}

#[test]
fn a_fresh_agent_in_the_tab_ends_the_alarm_of_a_move_that_stopped_after_its_exit() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(7),
        },
        LiveTab {
            sid: "s-0ther".to_string(),
            fgpgid: Some(8),
        },
    ];
    let shown_with = |name: &str, held: Holder| {
        let dir = ScratchDir::new("upgrade-status", name);
        let o = opts(&dir);
        fresh_claude_state(&o, now_s() - 60);
        let script =
            move |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(vec![held.clone()]) };
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let mut view = View::default();
        view.refresh_with(&o, &tabs, now_s(), &script, &mut |r| sent.push(r.to_vec()));
        sent.last().map_or(0, Vec::len)
    };
    let leads = |tab: &str| Holder {
        led: true,
        ..holder(OTHER, "2.1.283", Some(tab))
    };
    assert_eq!(
        shown_with("lc2-fresh-here", leads(TAB)),
        0,
        "a fresh agent leads the tab: the row's ask is answered"
    );
    assert_eq!(
        shown_with(
            "lc2-fresh-codex",
            Holder {
                led: true,
                ..holder(
                    &super::super::codex_alab_drive::key(TAB),
                    "0.50.0",
                    Some(TAB)
                )
            }
        ),
        0,
        "the tab's Codex TUI leads it: the tab's agent is back"
    );
    assert_eq!(
        shown_with("lc2-fresh-elsewhere", leads("s-0ther")),
        1,
        "CONTROL: an agent in another tab"
    );
    assert_eq!(
        shown_with("lc2-fresh-env", holder(OTHER, "2.1.283", Some(TAB))),
        1,
        "CONTROL: proven by its environment alone"
    );
    assert_eq!(
        shown_with(
            "lc2-fresh-aside",
            Holder {
                aside: true,
                ..holder(OTHER, "2.1.283", Some(TAB))
            }
        ),
        1,
        "CONTROL: one not in front of its terminal"
    );
}

#[cfg(unix)]
#[test]
fn the_holders_read_names_the_agent_leading_a_fresh_restarts_tab_whatever_it_holds() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "tab-leader");
    let o = opts(&dir);
    let session = fresh_claude_state(&o, now_s() - 60);
    let mut agent = parked_agent(TAB);
    let pid = agent.id();
    register_as(&o.home, pid, OTHER, "2.1.300");
    let vetted: BTreeSet<String> = std::iter::once(session).collect();
    let leads = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(pid)),
    }];
    let elsewhere = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(1),
    }];
    let led = holders(&o, &vetted, Some(&leads));
    let no_roster = holders(&o, &vetted, None);
    let not_leading = holders(&o, &vetted, Some(&elsewhere));
    let _ = agent.kill();
    let _ = agent.wait();
    let named = |held: Option<Vec<Holder>>| {
        held.expect("the files read")
            .into_iter()
            .filter(|h| h.session == OTHER && h.led)
            .map(|h| h.tab)
            .collect::<Vec<_>>()
    };
    assert_eq!(named(led), [Some(TAB.to_string())], "the tab's leader");
    assert!(named(no_roster).is_empty(), "CONTROL: no roster");
    assert!(named(not_leading).is_empty(), "CONTROL: it leads no tab");
}

#[test]
fn a_resume_rows_alarm_stands_while_another_conversation_leads_its_tab() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let dir = ScratchDir::new("upgrade-status", "review-lc2-resume");
    let o = opts(&dir);
    let session = moved_state(
        &o,
        upgrade::Agent::Claude,
        Phase::Failed("no-resume".to_string()),
        now_s() - 60,
    );
    let rows_with = |held: Vec<Holder>| {
        let script = move |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(held.clone()) };
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let mut view = View::default();
        view.refresh_with(&o, &tabs, now_s(), &script, &mut |r| sent.push(r.to_vec()));
        sent.last().cloned().unwrap_or_default()
    };
    let alone = rows_with(Vec::new());
    assert_eq!(alone.len(), 1);
    assert!(!alone[0].fresh, "a resume row");
    assert!(
        format!("{:?}", alone[0].attention(now_s())).contains("--resume"),
        "it asks for that conversation"
    );
    assert_eq!(
        rows_with(vec![holder(OTHER, "2.1.283", Some(TAB))]).len(),
        1,
        "another conversation leads the tab: the alarm stands"
    );
    assert_eq!(
        rows_with(vec![Holder {
            led: true,
            ..holder(
                &super::super::codex_alab_drive::key(TAB),
                "0.50.0",
                Some(TAB)
            )
        }])
        .len(),
        1,
        "the tab's Codex TUI leads it: the alarm stands"
    );
    assert!(
        rows_with(vec![holder(&session, "2.1.283", Some(TAB))])
            .iter()
            .all(|r| !r.failed_after_exit()),
        "its own conversation answers it"
    );
}

#[cfg(unix)]
#[test]
fn the_holders_read_a_tabs_leader_only_for_a_fresh_restarts_row_there() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let dir = ScratchDir::new("upgrade-status", "tab-leader-resume");
    let o = opts(&dir);
    let session = moved_state(
        &o,
        upgrade::Agent::Claude,
        Phase::Failed("no-resume".to_string()),
        now_s() - 60,
    );
    let mut agent = parked_agent(TAB);
    let pid = agent.id();
    register_as(&o.home, pid, OTHER, "2.1.300");
    let vetted: BTreeSet<String> = std::iter::once(session).collect();
    let leads = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(pid)),
    }];
    let held = holders(&o, &vetted, Some(&leads));
    let _ = agent.kill();
    let _ = agent.wait();
    let named: Vec<Option<String>> = held
        .expect("the files read")
        .into_iter()
        .filter(|h| h.session == OTHER)
        .map(|h| h.tab)
        .collect();
    assert!(named.is_empty(), "a resume row's tab: {named:?}");
}

#[test]
fn a_tab_whose_agent_is_switched_off_reads_its_switch_never_aterms_own_fault() {
    let dir = ScratchDir::new("upgrade-status", "agent-off");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 7 * 3_600,
        looked_at: NOW - 3 * 3_600,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &st, NOW - 7 * 3_600);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    pin_awake_since(NOW - 86_400);
    let strings = |tabs: &[&str]| tabs.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
    let look = |claimed: &[&str], agent_off: &[&str], off: &[&str]| {
        let mut view = View::default();
        view.set_claimed(&strings(claimed));
        view.set_agent_off(&strings(agent_off));
        view.set_off(&strings(off));
        let mut sent: Vec<Row> = Vec::new();
        let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
        sent.remove(0)
    };
    for agent_off in [&[][..], &["s-another"][..]] {
        assert_eq!(
            look(&[], agent_off, &[]).stall(NOW).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {agent_off:?}"
        );
    }
    let row = look(&[], &[TAB], &[]);
    assert_eq!(
        (row.aside.as_str(), row.wait.as_str()),
        (AGENT_OFF, AGENT_OFF)
    );
    assert_eq!(row.unwatched(NOW), None);
    assert!(
        row.badge(NOW).is_none(),
        "the owner's own switch marks no tab"
    );
    assert!(
        matches!(row.attention(NOW), Attention::Quiet),
        "the owner's own setup, a record: {:?}",
        row.attention(NOW)
    );
    assert_eq!(row.overdue_cause(NOW), "its agent is switched off");
    assert!(
        row.waiting_line("09:00")
            .contains("it moves once its agent is switched on again"),
        "{}",
        row.waiting_line("09:00")
    );
    let both = look(&[TAB], &[TAB], &[]);
    assert_eq!(
        (both.aside.as_str(), both.wait.as_str()),
        (CLAIMED, CLAIMED)
    );
    let switch_and_word = look(&[], &[TAB], &[TAB]);
    assert_eq!(switch_and_word.aside.as_str(), AGENT_OFF);
}

#[test]
fn a_recorded_line_ending_cd_reads_to_its_end_and_the_view_stands() {
    use crate::harness::accounts::LOGIN_ENV;
    for line in ["cd --", "true; cd --", "( cd --", "X=1 cd --"] {
        let last = last_command(line);
        assert!(program_at(&last) <= last.len(), "{line}: {last:?}");
        assert_eq!(resume_login(line), None, "{line}");
        assert_eq!(home_exports(line), Vec::new(), "{line}");
        assert_eq!(launch_of(line), None, "{line}");
    }
    assert_eq!(
        resume_login(&format!(
            "( cd -- '/x' && {LOGIN_ENV}='/l' exec claude --resume a )"
        ))
        .as_deref(),
        Some("/l")
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    for line in ["cd --", ""] {
        let dir = ScratchDir::new("upgrade-status", "cd-dashdash");
        let o = opts(&dir);
        save(
            &o,
            "0badf00d-1111-2222-3333-444455556666",
            &St {
                phase: Phase::Failed("stale-exit".to_string()),
                from: "2.1.281".to_string(),
                to: "2.1.283".to_string(),
                source: "managed".to_string(),
                tab: TAB.to_string(),
                pending_since: now_s() - 600,
                exited_at: now_s() - 120,
                line: line.to_string(),
                ..St::default()
            },
        );
        let mut view = View::default();
        let mut sent: Vec<Vec<Row>> = Vec::new();
        view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |r| sent.push(r.to_vec()));
        let rows = sent.last().expect("the view sent its rows");
        assert_eq!(rows.len(), 1, "{line:?}: the row stands");
        let _ = rows[0].interrupt(now_s());
    }
}

#[test]
fn one_record_whose_read_panics_is_an_unreadable_record() {
    for panics in [true, false] {
        let dir = ScratchDir::new("upgrade-status", "record-panics");
        let o = opts(&dir);
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        let st = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: NOW - 60,
            ..St::default()
        };
        super::super::save_at(&o, "aaa", &st, NOW);
        if panics {
            super::super::save_at(
                &o,
                RECORD_PANICS,
                &St {
                    tab: "s-faulty".to_string(),
                    ..st.clone()
                },
                NOW,
            );
        }
        let tabs = [
            LiveTab {
                sid: TAB.to_string(),
                fgpgid: Some(7),
            },
            LiveTab {
                sid: "s-faulty".to_string(),
                fgpgid: Some(8),
            },
        ];
        let held = behind_in(TAB);
        let mut view = View::default();
        let due = NOW + upgrade::REASK_S;
        let mut sent: Vec<(Vec<String>, Vec<String>)> = Vec::new();
        let mut hand = |rows: &[Row], kept: &[String]| {
            sent.push((
                rows.iter().map(|r| r.session.clone()).collect(),
                kept.to_vec(),
            ));
        };
        assert_eq!(
            view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut hand),
            Some(due),
            "panics={panics}: the readable record's watch is the instant named"
        );
        let _ = view.refresh_with_keeping(&o, &tabs, due, &held, &mut hand);
        assert_eq!(
            view.watch_due(),
            [TAB],
            "panics={panics}: the readable record's tab is watched once due"
        );
        let kept: Vec<String> = if panics {
            vec!["s-faulty".to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            sent.first(),
            Some(&(vec!["aaa".to_string()], kept)),
            "panics={panics}: the readable record's row is handed over, the faulty one's tab \
             left as it stands"
        );
    }
}

#[test]
fn an_agys_need_names_a_command_that_runs() {
    let agy = Row {
        agent: upgrade::Agent::Agy,
        ..Row::default()
    };
    let down = agy.agent_down("no-resume");
    let (then, command) = agy.resume_said("Resume it in that tab with this command");
    for (then, command) in [(&down.then, &down.command), (&then, &command)] {
        assert_eq!(command.as_deref(), Some("agy"), "{then}");
        assert!(then.ends_with("then /resume"), "{then}");
        assert!(then.chars().count() <= NEED_THEN_MAX, "{then}");
    }
    let muse = Row {
        agent: upgrade::Agent::Muse,
        ..Row::default()
    };
    assert_eq!(
        muse.resume_said("Resume it in that tab with this command")
            .1
            .as_deref(),
        Some("muse resume"),
        "CONTROL"
    );
}

#[cfg(unix)]
#[test]
fn a_record_that_cannot_be_read_any_more_leaves_its_tab_as_it_stands() {
    use std::os::unix::fs::PermissionsExt as _;
    let pending = |tab: &str| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: tab.to_string(),
        pending_since: NOW - 60,
        ..St::default()
    };
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(7),
        },
        LiveTab {
            sid: "s-faulty".to_string(),
            fgpgid: Some(8),
        },
    ];
    let held = |sessions: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(
            sessions
                .iter()
                .map(|s| {
                    let tab = if s == "aaa" { TAB } else { "s-faulty" };
                    holder(s, "2.1.281", Some(tab))
                })
                .collect(),
        )
    };
    for fault in ["panics", "unreadable"] {
        let dir = ScratchDir::new("upgrade-status", "record-unread");
        let o = opts(&dir);
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        super::super::save_at(&o, "aaa", &pending(TAB), NOW);
        let faulty = if fault == "panics" {
            RECORD_PANICS
        } else {
            "fff"
        };
        let mut view = View::default();
        type Handed = (Vec<(String, String)>, Vec<String>);
        let sent: std::cell::RefCell<Vec<Handed>> = std::cell::RefCell::default();
        let mut hand = |rows: &[Row], kept: &[String]| {
            sent.borrow_mut().push((
                rows.iter()
                    .map(|r| (r.session.clone(), r.tab.clone()))
                    .collect(),
                kept.to_vec(),
            ));
        };
        if fault == "panics" {
            super::super::save_at(&o, RECORD_PANICS, &pending("s-faulty"), NOW);
        } else {
            super::super::save_at(&o, faulty, &pending("s-faulty"), NOW);
        }
        let _ = view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut hand);
        if fault == "unreadable" {
            let path = state_dir(&o).join("fff.json");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
                .expect("unreadable");
        }
        let done = St {
            phase: Phase::Done,
            done_at: NOW - 3 * DONE_SHOWN_S,
            ..pending(TAB)
        };
        super::super::save_at(&o, "aaa", &done, NOW);
        sent.borrow_mut().clear();
        let _ = view.refresh_with_keeping(&o, &tabs, NOW + 1, &held, &mut hand);
        assert_eq!(
            *sent.borrow(),
            [(Vec::new(), vec!["s-faulty".to_string()])],
            "{fault}: the readable row renewed, the faulty one's tab left as it stands"
        );
        let _ = std::fs::set_permissions(
            state_dir(&o).join("fff.json"),
            std::fs::Permissions::from_mode(0o644),
        );
    }
    for unread in [true, false] {
        let dir = ScratchDir::new("upgrade-status", "record-unread-mark");
        let (sock, asked) = recording_socket(&dir);
        let o = Opts {
            sock: Some(sock),
            ..opts(&dir)
        };
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        super::super::save_at(&o, "aaa", &pending(TAB), NOW);
        if unread {
            super::super::save_at(&o, RECORD_PANICS, &pending("s-faulty"), NOW);
        }
        let mut view = View::default();
        let mut sent: Vec<(usize, Vec<String>)> = Vec::new();
        let _ = view.refresh_with_keeping(&o, &tabs, NOW, &held, &mut |rows, kept| {
            sent.push((rows.len(), kept.to_vec()));
        });
        let kept: Vec<String> = if unread {
            vec!["s-faulty".to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            sent,
            [(1, kept)],
            "unread={unread}: the readable record's row, the faulty tab left as it stands"
        );
        let lowered = asked
            .lock()
            .map(|a| {
                a.iter()
                    .any(|l| l == "@s-faulty meta unset attention owner=upgrade")
            })
            .unwrap_or(false);
        assert_eq!(
            lowered, !unread,
            "unread={unread}: the faulty tab's mark is settled only with nothing unread"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_record_this_look_cannot_read_neither_raises_nor_lowers_its_tabs_mark() {
    use std::os::unix::fs::PermissionsExt as _;
    let gone = |tab: &str| St {
        phase: Phase::Exiting { at_s: NOW - 10 },
        exited_at: NOW - 10,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: tab.to_string(),
        ..St::default()
    };
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(7),
        },
        LiveTab {
            sid: "s-faulty".to_string(),
            fgpgid: Some(8),
        },
    ];
    let landed = |sessions: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(
            sessions
                .iter()
                .map(|s| {
                    let tab = if s == "aaa" { TAB } else { "s-faulty" };
                    holder(s, "2.1.282", Some(tab))
                })
                .collect(),
        )
    };
    let later = NOW + crate::harness::relaunch::STALE_S + 60;
    for (fault, marked) in [
        ("unreadable", false),
        ("panics", false),
        ("unreadable", true),
    ] {
        let case = format!("{fault} marked={marked}");
        let dir = ScratchDir::new("upgrade-status", "unread-mark");
        let (sock, asked) = recording_socket(&dir);
        let o = Opts {
            sock: Some(sock),
            ..opts(&dir)
        };
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        super::super::save_at(&o, "aaa", &gone(TAB), NOW);
        let session = if fault == "panics" {
            RECORD_PANICS
        } else {
            "fff"
        };
        super::super::save_at(&o, session, &gone("s-faulty"), NOW);
        let mut view = View::default();
        if marked {
            let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
            let _ = view.refresh_with(&o, &tabs, later, &nobody, &mut |_| {});
            assert!(view.badged.contains_key("s-faulty"), "{case}: marked");
        } else {
            let _ = view.refresh_with(&o, &tabs, NOW, &landed, &mut |_| {});
            assert!(!view.badged.contains_key("s-faulty"), "{case}: unmarked");
        }
        let path = state_dir(&o).join(format!("{session}.json"));
        if fault == "unreadable" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
                .expect("unreadable");
        }
        asked.lock().expect("log").clear();
        let _ = view.refresh_with_keeping(&o, &tabs, later + 1, &landed, &mut |_, _| {});
        let lines: Vec<String> = asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter(|l| l.starts_with("@s-faulty meta "))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            lines.is_empty(),
            "{case}: no mark raised or lowered off a row no read gave this look: {lines:?}"
        );
        assert_eq!(
            view.badged.contains_key("s-faulty"),
            marked,
            "{case}: its mark as it stood"
        );
        assert_eq!(
            view.sent_kept,
            ["s-faulty".to_string()],
            "{case}: only the faulty tab left as it stands"
        );
        if fault == "unreadable" {
            std::fs::remove_file(&path).expect("the unreadable copy");
            let done = St {
                phase: Phase::Done,
                done_at: later,
                ..gone("s-faulty")
            };
            super::super::save_at(&o, session, &done, later);
            let _ = view.refresh_with(&o, &tabs, later + 2, &landed, &mut |_| {});
            assert!(
                !view.badged.contains_key("s-faulty"),
                "{case}: CONTROL: read again, landed, no mark"
            );
        }
    }
    for unread in [true, false] {
        let dir = ScratchDir::new("upgrade-status", "unread-mark-other");
        let (sock, asked) = recording_socket(&dir);
        let o = Opts {
            sock: Some(sock),
            ..opts(&dir)
        };
        std::fs::create_dir_all(state_dir(&o)).expect("state");
        let down = St {
            phase: Phase::Failed("stale-exit".to_string()),
            exited_at: later - 600,
            ..gone("s-faulty")
        };
        super::super::save_at(&o, "ggg", &down, later);
        if unread {
            super::super::save_at(&o, RECORD_PANICS, &gone("s-faulty"), NOW);
        }
        let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
        let mut view = View::default();
        let _ = view.refresh_with(&o, &tabs, later, &nobody, &mut |_| {});
        let raised = asked.lock().is_ok_and(|a| {
            a.iter()
                .any(|l| l.starts_with("@s-faulty meta set attention owner=upgrade"))
        });
        assert_eq!(
            raised, !unread,
            "unread={unread}: raised only with nothing unread"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unread_record_no_tab_is_known_for_keeps_its_conversations_tab() {
    let dir = ScratchDir::new("upgrade-status", "unread-unplaced");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let pending = |tab: &str| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: tab.to_string(),
        pending_since: NOW - 60,
        ..St::default()
    };
    super::super::save_at(&o, "aaa", &pending(TAB), NOW);
    super::super::save_at(&o, "ccc", &pending("s-two"), NOW);
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(7),
        },
        LiveTab {
            sid: "s-two".to_string(),
            fgpgid: Some(8),
        },
    ];
    let looped = state_dir(&o).join("bbb.json");
    std::os::unix::fs::symlink(&looped, &looped).expect("a record that loops");
    let in_two = |sessions: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(
            sessions
                .iter()
                .map(|s| {
                    let tab = if s == "aaa" { TAB } else { "s-two" };
                    holder(s, "2.1.281", Some(tab))
                })
                .collect(),
        )
    };
    let unheld = |sessions: &BTreeSet<String>| -> Option<Vec<Holder>> {
        Some(
            sessions
                .iter()
                .filter(|s| *s != "bbb")
                .map(|s| {
                    let tab = if s == "aaa" { TAB } else { "s-two" };
                    holder(s, "2.1.281", Some(tab))
                })
                .collect(),
        )
    };
    let keeps = |view: &mut View, holders: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        let mut kept: Vec<Vec<String>> = Vec::new();
        let _ = view.refresh_with_keeping(&o, &tabs, NOW, holders, &mut |_, k| {
            kept.push(k.to_vec());
        });
        kept.pop().unwrap_or_default()
    };
    let mut view = View::default();
    assert_eq!(
        keeps(&mut view, &in_two),
        ["s-two".to_string()],
        "the tab its conversation's process runs in, alone"
    );
    assert_eq!(view.unreadable(), std::slice::from_ref(&looped), "named");
    assert_eq!(
        keeps(&mut view, &unheld),
        [TAB.to_string(), "s-two".to_string()],
        "no process holds it in a tab: every live tab"
    );
    assert_eq!(view.unreadable(), std::slice::from_ref(&looped));
    std::fs::remove_file(&looped).expect("unloop");
    assert!(keeps(&mut view, &in_two).is_empty(), "read whole");
    assert!(view.unreadable().is_empty(), "read whole: none named");
    super::super::save_at(&o, RECORD_PANICS, &pending("s-closed"), NOW);
    assert!(keeps(&mut view, &in_two).is_empty(), "its tab closed");
    assert!(view.unreadable().is_empty(), "its tab closed: none named");
    super::super::save_at(&o, RECORD_PANICS, &pending("s-two"), NOW);
    let _ = keeps(&mut view, &in_two);
    assert_eq!(view.unreadable().len(), 1);
    view.stand_down(&mut |_| {});
    assert!(view.unreadable().is_empty(), "stood down: none named");
}

#[cfg(unix)]
#[test]
fn an_answered_down_record_stays_answered_across_an_unread_look() {
    let dir = ScratchDir::new("upgrade-status", "answered-unread");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let fresh = crate::harness::relaunch::CAUSE_UPGRADE_FRESH;
    let exited = NOW - 120;
    let st = St {
        phase: Phase::Failed("no-resume".to_string()),
        cause: fresh.to_string(),
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        proc_start: "Mon Oct 26 12:00:00 2026".into(),
        exited_at: exited,
        ..St::default()
    };
    let session = "0badf00d-1111-2222-3333-444455556666";
    super::super::save_at(&o, session, &st, NOW);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| Some(Vec::<Holder>::new());
    let owner_back = |_: &BTreeSet<String>| {
        Some(vec![Holder {
            led: true,
            proc_start: "Mon Oct 26 12:01:00 2026".into(),
            ..holder(NEW, "2.1.283", Some(TAB))
        }])
    };
    let look = |view: &mut View, held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        let mut shown = None;
        let _ = view.refresh_with_keeping(&o, &tabs, NOW, held, &mut |rows, _| {
            shown = Some(rows.to_vec())
        });
        shown
    };
    let mut view = View::default();
    assert_eq!(
        look(&mut view, &nobody).unwrap().len(),
        1,
        "ordinary unanswered row"
    );
    assert!(
        look(&mut view, &owner_back).unwrap().is_empty(),
        "owner answered"
    );
    assert_eq!(view.back.get(session), Some(&exited));
    let path = state_dir(&o).join(format!("{session}.json"));
    let saved = dir.join("held-record");
    std::fs::rename(&path, &saved).expect("keep record");
    std::os::unix::fs::symlink(&path, &path).expect("deterministic unreadable record");
    let _ = look(&mut view, &nobody);
    std::fs::remove_file(&path).expect("remove unreadable link");
    std::fs::rename(&saved, &path).expect("readable again");
    let shown = look(&mut view, &nobody).unwrap();
    assert!(
        shown.is_empty(),
        "ANSWERED_RECORD_MUST_SURVIVE_UNREAD_LOOK: {shown:?}"
    );
    assert_eq!(view.back.get(session), Some(&exited));
    let mut restarted = View::default();
    assert!(
        look(&mut restarted, &nobody).unwrap().is_empty(),
        "answer still persisted"
    );
    super::super::save_at(
        &o,
        session,
        &St {
            phase: Phase::Done,
            done_at: NOW,
            ..st.clone()
        },
        NOW,
    );
    let _ = look(&mut view, &nobody);
    assert!(!view.back.contains_key(session));
    super::super::save_at(&o, session, &st, NOW);
    assert_eq!(
        look(&mut view, &nobody).unwrap().len(),
        1,
        "new down state is shown"
    );
}

/// A round the drive stops because someone else resumed its conversation on an older build
/// (`failed:resumed-by-another`) reads as that stop in its status row, not as the words for a stop
/// nobody named; the control: a stop nobody named reads those.
#[test]
fn a_stop_for_a_resume_by_someone_else_reads_as_its_own() {
    use crate::harness::upgrade_drive::RESUMED_BEHIND;
    let claude = upgrade::Agent::Claude;
    let unnamed = stall_reason(claude, "failed:a-stop-nobody-named");
    let ours = stall_reason(claude, &format!("failed:{RESUMED_BEHIND}"));
    assert_ne!(ours, unnamed, "{ours}");
    assert_ne!(ours, stall_reason(claude, "failed:resumed-elsewhere"));
    assert_eq!(
        unnamed,
        stop_words(claude, "another-stop-nobody-named"),
        "control: the words for a stop nobody named"
    );
}
