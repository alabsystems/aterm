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
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: true,
        background: false,
        aterm_state: None,
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`).
        ask: true,
        several_logins: false,
    }
}

/// A live holder of `session` on `version`, proven in `tab`.
fn holder(session: &str, version: &str, tab: Option<&str>) -> Holder {
    Holder {
        session: session.to_string(),
        version: version.to_string(),
        tab: tab.map(str::to_string),
        ..Holder::default()
    }
}

/// Scripted holders for [`View::refresh_with`]: every conversation
/// asked about is held in its recorded tab on 2.1.281 — the live, behind case.
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

/// A native 2.1.281 behind managed 2.1.282 since 13:09, waiting on a busy turn.
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

/// Pending is not stalled: a session waiting for its turn end marks neither
/// tab nor band. It reads stalled once [`STALLED_AFTER_S`] behind, when the
/// agent runs where typing cannot reach, and when the upgrade stopped, unless
/// the user's own word holds it.
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
    let measured = pending(8 * 3_600 + 22 * 60);
    assert_eq!(
        measured.stall_words(NOW).as_deref(),
        Some("behind for 8 h: its turn is still running")
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
    // A round that gave up is no stall of its own: it rests and the next round
    // asks again. Its age still reads `overdue`, as every round's does, so a
    // long-behind tab says the same word throughout. Resting, it is not
    // `moving`: it moves only after the rest, a notice and a READY.
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
    // The owner's word holds it: no stall while it holds.
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
    let deferred = Row {
        request: Request::DeferUntil(NOW + 60),
        ..old.clone()
    };
    assert_eq!(deferred.stall(NOW), None);
    assert_eq!(deferred.stall(NOW + 60).as_deref(), Some("overdue"));
    // In flight or done is never stalled.
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

/// The roster word: `<state>/<to>/<why>/<age>`, one token.
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
    // `--status`'s sentence: what holds the move, in words.
    for (row, why) in [
        (&deferred, "deferred by the owner's word for 1m"),
        (&skipped, "skipped by the owner's word"),
        (&restarting, "its move is under way"),
        (&done, "done"),
    ] {
        assert_eq!(
            row.summary(NOW),
            format!("Claude Code 2.1.281 → 2.1.282: {why}")
        );
    }
    // A restart that has not moved is said stuck, never "under way".
    let stuck = Row {
        phase: Phase::Exiting {
            at_s: NOW - super::super::STALE_S - 1,
        },
        ..pending(120)
    };
    assert!(
        stuck.summary(NOW).starts_with(
            "Claude Code 2.1.281 → 2.1.282: its restart is stuck: it was asked to end"
        ),
        "{}",
        stuck.summary(NOW)
    );
    for row in [pending(1), asked, deferred, skipped, restarting, done] {
        let col = row.column(NOW);
        assert!(!col.contains(char::is_whitespace), "{col}");
        assert_eq!(col.split('/').count(), 4, "{col}");
    }
}

/// The view and the drive read one word after a gone notice: a `--skip` or
/// `--defer` for the notice's tab keeps its fence when the new round moves the
/// record to the holder's tab ([`St::notice_gone`]). Neither the drive
/// ([`St::request_for`]) nor the row ([`Row::of`]) honours it there. Negative
/// control: a holder in the named tab is held in both.
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

/// A restart whose carry-on is typed is `restarting/<to>/continued/<behind>`
/// while its model is awaited: the typed path saves it done with no `done_at`,
/// and until `confirm_by` it is handed over. Past it, model still unread, the
/// row is `done`, aged from `confirm_by`, and the view names the instant it
/// turns; with the model read, `done` from then. Negative control: a done
/// record with no `done_at` that owes nothing is dropped.
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
    // Past the model's wait, the carry-on's answer still running: done.
    view.refresh_with(&o, &tabs, now + 125, &held, &mut |rows| {
        sent.push(rows.to_vec())
    });
    assert_eq!(
        sent.last().expect("sent")[0].column(now + 125),
        "done/2.1.282/-/5s"
    );
    // Its model read: done, from then.
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
    // Negative control: done, no `done_at`, nothing owed.
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

/// `--status` reads every state file, filtered to one tab when asked, and
/// carries how long and why.
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
    assert_eq!(
        only[0].line(NOW),
        format!(
            "upgrade tab={TAB} session=aaa from=2.1.281 to=2.1.282(managed) phase=pending \
             pending_for=8h22m wait=not-idle:busy wait_for=8h20m request=- next_round=- \
             stalled=overdue held_by=- release=- rung=land looked=- by=- point=- guard=- \
             watch_at=15h38m cause=own-work told=-"
        )
    );
    let json = only[0].to_json(NOW);
    assert_eq!(
        json.get("pending_for_s").and_then(Value::as_u64),
        Some(30_120)
    );
    assert_eq!(json.get("stalled").and_then(Value::as_str), Some("overdue"));
}

/// The user's word lands in the tab's upgrade under the sweep lock with a
/// ledger line; `--now` re-arms a stopped upgrade; a restart under way and a
/// tab with nothing recorded are refused; a lock another sweep holds is
/// `busy:another-sweep`, writing nothing.
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

    // A stopped upgrade: `--now` re-arms it, and the newest live one wins.
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

    // Refused or failed is not re-armed by `--now` (nothing is written);
    // `--skip` still ends it.
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
        // As written: the one writer stamps its progress (`St::progress_at`).
        let stopped = load(&o, "aaa").expect("saved");
        let e = ask(&o, TAB, Ask::Now, false).expect_err("not re-armed");
        assert!(e.contains(&format!("stopped ({why})")), "{why}: {e}");
        // Never "for good": it starts a new round at its next look.
        assert!(!e.contains("for good"), "{why}: {e}");
        assert!(
            e.contains("starts a new round on its own at its next look"),
            "{why}: {e}"
        );
        assert!(e.contains("--skip"), "{why}: {e}");
        assert_eq!(load(&o, "aaa"), Some(stopped), "{why}: nothing written");
        let row = ask(&o, TAB, Ask::Skip, false).expect("a skip ends it");
        assert_eq!(row.stall(now_s()), None, "{why}");
    }
    save(&o, "aaa", &st);

    // Nothing recorded for another tab: refused, nothing written.
    let e = ask(&o, "s-0000", Ask::Now, false).expect_err("no upgrade there");
    assert!(e.contains("no upgrade is recorded"), "{e}");

    // A restart under way is neither held nor hurried.
    save(
        &o,
        "ccc",
        &St {
            phase: Phase::Exiting { at_s: NOW },
            tab: "s-busy".to_string(),
            ..st.clone()
        },
    );
    let e = ask(&o, "s-busy", Ask::Defer(60), false).expect_err("in flight");
    assert!(e.contains("already under way"), "{e}");
    assert_eq!(
        load(&o, "ccc").expect("state").request,
        Request::None,
        "nothing written"
    );

    // Another sweep holds the lock: `busy`, nothing written.
    let held = super::super::sweep_lock(&o).expect("lock").expect("held");
    let before = load(&o, "aaa");
    let wait = std::time::Duration::from_millis(300);
    let started = std::time::Instant::now();
    let e = ask_within(&o, TAB, Ask::Skip, None, wait).expect_err("busy");
    assert_eq!(e, "busy:another-sweep");
    assert!(started.elapsed() >= wait, "it waited its bound");
    assert_eq!(load(&o, "aaa"), before, "nothing written while busy");
    drop(held);
}

/// A word for a build the user was shown ([`ask_for`]) lands nothing once the
/// upgrade moved to a newer target: nothing written, ledgered or announced.
/// Negative control: the same words for the current target land
/// (`2.1.283.0` is the same build).
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
        let e = ask_for(&o, TAB, word, "2.1.282").expect_err("the row moved on");
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

    let row = ask_for(&o, TAB, Ask::Skip, "2.1.283").expect("the build shown");
    assert_eq!(row.request, Request::Skip("2.1.283".to_string()));
    let row = ask_for(&o, TAB, Ask::Now, "2.1.283.0").expect("the same build");
    assert_eq!(row.request, Request::Now);
    assert_eq!(
        load(&o, "aaa").expect("state").request,
        Request::Now,
        "written"
    );
}

/// The user's word is a push: a word that lands replaces the [`word_marker`]
/// file, so the host's activation wake sees it change and the tab's worker
/// takes the word at its next idle point. Negative control: a refused word
/// replaces nothing.
#[test]
fn an_owners_word_that_lands_wakes_the_window() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = ScratchDir::new("upgrade-status", "word-push");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let marker = word_marker(&o.state);
    assert!(!marker.exists(), "no word yet");
    let e = ask(&o, TAB, Ask::Now, false).expect_err("nothing recorded");
    assert!(e.contains("no upgrade is recorded"), "{e}");
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

/// Relaunch records are no upgrades: a relaunched or restarted-in-place agent's
/// record (`St::cause`) is neither shown as an upgrade nor takes a word.
/// Negative control: the upgrade's own record beside it is shown and takes the
/// word.
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
    let e = ask(&o, TAB, Ask::Now, false).expect_err("no upgrade there");
    assert!(e.contains("no upgrade is recorded"), "{e}");
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

/// The view names when it next changes by time alone (turning overdue, a
/// `--now` expiring, a `--defer` running out, a finished move leaving the
/// summary), the earliest to come, so the host needs no timer. Negative
/// controls: an overdue row with nothing to come and a restart in flight name
/// nothing.
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
    // Already overdue, and busy with its own work: the next thing that reads
    // otherwise by time alone is the digest's day (`Row::attention`).
    assert_eq!(
        next_change(&[pending(7 * 3_600)], NOW),
        Some(NOW - 7 * 3_600 + DIGEST_AFTER_S),
        "already overdue: the digest's day"
    );
    assert_eq!(
        next_change(
            &[Row {
                wait: "box".into(),
                ..pending(7 * 3_600)
            }],
            NOW
        ),
        None,
        "already overdue, waiting on nothing that turns into news by time"
    );
    // A suspended agent, a draft unsent, an agent asked to end and alive, an
    // agent gone with its resume line not typed: each names its instant.
    for (wait, want) in [
        ("not-foreground", NOW - 60 + SUSPENDED_AFTER_S),
        ("draft", NOW - 60 + DIGEST_AFTER_S),
    ] {
        let r = Row {
            wait: wait.into(),
            wait_since: NOW - 60,
            ..pending(7 * 3_600)
        };
        assert_eq!(r.next_attention_at(NOW), Some(want), "{wait}");
    }
    let hung = Row {
        phase: Phase::Exiting { at_s: NOW - 60 },
        ..pending(60)
    };
    assert_eq!(
        hung.next_attention_at(NOW),
        Some(NOW - 60 + HUNG_EXIT_AFTER_S + 1)
    );
    let gone = Row {
        exited_at: NOW - 30,
        ..hung
    };
    assert_eq!(
        gone.next_attention_at(NOW),
        Some(NOW - 30 + super::super::STALE_S + 1)
    );
    // A wait no rung passes turns `blocked` by time alone (ruling 380), on
    // its blocker's own clock.
    let blocking = Row {
        wait: "daemon-turn".into(),
        wait_since: NOW - 10,
        blocked: "no-shell-integration".into(),
        blocked_since: NOW - 60,
        ..pending(60)
    };
    assert_eq!(
        next_change(&[blocking], NOW),
        Some(NOW - 60 + BLOCKED_AFTER_S),
        "turning blocked"
    );
    // A restart under way turns `stuck` at its bound, by time alone, then
    // changes no more.
    let restarting = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(60)
    };
    assert_eq!(
        next_change(&[restarting], NOW),
        Some(NOW + super::super::STALE_S + 1)
    );
    // A relaunched agent that reaches no idle point turns a fault line at its
    // own bound (`RELAUNCH_STUCK_S`), by time alone — and changes no more.
    let stuck = Row {
        phase: Phase::Relaunched {
            at_s: NOW - super::super::STALE_S - 1,
        },
        ..pending(60)
    };
    assert_eq!(
        next_change(std::slice::from_ref(&stuck), NOW),
        Some(NOW - super::super::STALE_S - 1 + RELAUNCH_STUCK_S)
    );
    let stuck = Row {
        phase: Phase::Relaunched {
            at_s: NOW - RELAUNCH_STUCK_S - 1,
        },
        ..pending(60)
    };
    // ... and an interrupt at twice that (`RELAUNCH_INTERRUPT_S`), then no more.
    assert_eq!(
        next_change(std::slice::from_ref(&stuck), NOW),
        Some(NOW - RELAUNCH_STUCK_S - 1 + RELAUNCH_INTERRUPT_S)
    );
    let stuck = Row {
        phase: Phase::Relaunched {
            at_s: NOW - RELAUNCH_INTERRUPT_S - 1,
        },
        ..pending(60)
    };
    assert_eq!(next_change(&[stuck], NOW), None);
    assert_eq!(next_change(&[], NOW), None);
}

/// A record's watch is an instant the view names, so the host wakes for it
/// without a timer, and once past, the look lists the tab due
/// ([`View::watch_due`]). Only live tabs, only once past, and by the records
/// alone. Negative controls: a row without the watch instant names nothing;
/// past it, it is no instant; before it, the tab is not due; a tab this
/// instance does not show is never listed.
///
/// A look that stops short still names the watch: each record's instant and
/// due status are its own, so an unreadable session file or a partly
/// unreadable record set does not drop them.
#[test]
fn the_view_names_a_records_watch_and_lists_it_due_once_past() {
    let watched = Row {
        watch_at: NOW + 600,
        ..pending(7 * 3_600)
    };
    assert_eq!(
        next_change(std::slice::from_ref(&watched), NOW),
        Some(NOW + 600),
        "the watch's instant, on an overdue row that names no other"
    );
    // Without the watch's arm only the classifier's own horizon (the day's
    // digest line, [`Row::next_attention_at`]) names an instant: the watch's
    // is not among them, and once it has passed, no instant of it remains.
    let unwatched = Row {
        watch_at: 0,
        ..watched.clone()
    };
    let horizon = unwatched.next_attention_at(NOW);
    assert!(horizon.is_some_and(|at| at > NOW + 600), "{horizon:?}");
    assert_eq!(
        next_change(std::slice::from_ref(&unwatched), NOW),
        horizon,
        "without the watch's arm nothing wakes the host for it before the horizon"
    );
    assert_eq!(
        next_change(std::slice::from_ref(&watched), NOW + 600),
        watched.next_attention_at(NOW + 600),
        "once past, the watch names no instant"
    );

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
    // The records decide it, not their holders: a look whose holders do not
    // read still lists the tab.
    let _ = view.refresh_with(&o, &tabs, due, &|_| None, &mut |_| {});
    assert_eq!(view.watch_due(), [TAB], "the records alone decide");
    let _ = view.refresh_with(&o, &[], due, &held, &mut |_| {});
    assert!(view.watch_due().is_empty(), "no live tab, none due");
    // A look whose records do not read whole still names the watch of
    // those that did: the instant before it comes, the tab once it has.
    let locked = state_dir(&o).join("unreadable.json");
    std::fs::write(&locked, "{}").expect("a record");
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000))
        .expect("chmod");
    // (Running as root reads a 0o000 file: then the look is whole anyway.)
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

/// A record nothing has checked, as the pure reading of its fields
/// ([`unwatched_since`]): its watch more than a gap late, no look for
/// [`UNWATCHED_AFTER_S`], the machine awake past [`WAKE_GRACE_S`]; then since
/// when (last look, or the watch it was due). [`unwatched_turns_at`] is exactly
/// the first second it reads so.
///
/// Negative controls: nothing flags within the wake grace; a record looked at
/// within the bound never flags; no watch owed (`watch_at` 0) or an unknown
/// awake clock never flags; the lateness gap keeps a stopped round from
/// flagging the second its rest ends.
#[test]
fn a_record_nothing_has_checked_reads_unwatched_past_the_wake_grace() {
    use crate::harness::upgrade_drive::WATCH_GAP;
    let long_awake = NOW - 86_400;
    // The stuck record the design was written for, watched at its gap.
    let looked = NOW - UNWATCHED_AFTER_S - 60;
    let due = looked + WATCH_GAP;
    assert_eq!(unwatched_since(due, looked, NOW, long_awake), Some(looked));
    // Never looked at: since the watch it was due.
    let first = NOW - WATCH_GAP - 60;
    assert_eq!(unwatched_since(first, 0, NOW, long_awake), Some(first));
    // Within the wake grace: nothing, to its last second.
    for woke in [NOW - 60, NOW - WAKE_GRACE_S + 1] {
        assert_eq!(
            unwatched_since(due, looked, NOW, woke),
            None,
            "woke {} s ago",
            NOW - woke
        );
    }
    assert_eq!(
        unwatched_since(due, looked, NOW, NOW - WAKE_GRACE_S),
        Some(looked),
        "the grace is over"
    );
    // Looked at within the bound: its watch is not due, or not a gap late.
    for looked_at in [NOW - 60, NOW - WATCH_GAP, NOW - UNWATCHED_AFTER_S] {
        assert_eq!(
            unwatched_since(looked_at + WATCH_GAP, looked_at, NOW, long_awake),
            None,
            "looked {} s ago",
            NOW - looked_at
        );
    }
    // Owed no watch, and no awake clock.
    assert_eq!(unwatched_since(0, looked, NOW, long_awake), None);
    assert_eq!(unwatched_since(due, looked, NOW, 0), None);
    // The design's bound alone: a round that stopped ten hours ago, looked at
    // then, its rest over a second ago.
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
    // The instant it turns, whichever bound decides: lateness, last look, or
    // wake grace.
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

/// The awake clock is the machine's last wake or boot ([`system_awake_since`]:
/// `kern.waketime`/`kern.boottime` on macOS, `/proc/stat` `btime` on Linux),
/// past and after 2001; a test build reads its pin ([`awake_since`], `0` unless
/// set), so no test depends on the machine.
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
    assert_eq!(awake_since(), 0, "a test reads its pin, not the machine");
    pin_awake_since(NOW - 60);
    assert_eq!(awake_since(), NOW - 60);
    pin_awake_since(0);
    assert_eq!(awake_since(), 0);
}

/// AN UNWATCHED RECORD IS TOLD AS ATERM'S OWN FAULT (design record 2026-09-28,
/// C6 and C10): a stall
/// ([`Row::stall`] `unwatched`) and ONE fault line in the log
/// ([`Row::attention`], kind `unwatched`) — never a row or the tab's mark,
/// since the owner has nothing to do about aterm's own timer — over what the
/// record's last look said — a healthy pending round, and every
/// round that would be a record (one asking on its own, one resting after a
/// give-up, a limit before its reset): no look has come since, so none of
/// that is known. Its words, within the band's 64 characters, say for how
/// long and whose fault; the line says the bound, never the running age, so
/// it is written once; `--status` says `stalled=unwatched` and `told=unwatched`,
/// and the `upgrade=` column for how long. No word of the owner's mends it
/// (`Upgrade now` is not offered, [`Remedy::Waits`]).
///
/// Negative controls: the same record within the wake grace, looked at within
/// the bound, or with an unknown awake clock reads its last look; finished,
/// restarting, stopped-after-exit and skipped records never read it.
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
    assert_eq!(words, "not checked for 1 h \u{2014} aterm's own fault");
    assert!(words.chars().count() <= 64, "{words}");
    assert_eq!(row.badge(NOW), None, "aterm's own fault marks no tab");
    let Attention::Fault(need) = row.attention(NOW) else {
        panic!("a fault line: {:?}", row.attention(NOW));
    };
    assert_eq!(need.kind, "unwatched");
    assert!(
        need.why.contains("more than 1h") && need.why.contains("aterm's own"),
        "{}",
        need.why
    );
    assert_eq!(
        row.attention(NOW + 3 * 3_600),
        row.attention(NOW),
        "the line says the bound, never the running age"
    );
    assert_eq!(row.remedy(NOW), Some(Remedy::Waits), "no word mends it");
    assert!(!row.moving(NOW));
    assert_eq!(row.column(NOW), "stalled/2.1.282/unwatched:1h10m/1h");
    let line = row.line(NOW);
    assert!(line.contains(" stalled=unwatched "), "{line}");
    assert!(line.contains(" told=unwatched"), "{line}");
    let json = row.to_json(NOW);
    assert_eq!(
        json.get("stalled").and_then(Value::as_str),
        Some("unwatched")
    );
    assert_eq!(json.get("told").and_then(Value::as_str), Some("unwatched"));
    // Its age is said, restated as it grows, at an instant the view names.
    assert_eq!(row.age_said(NOW).as_deref(), Some("1 h"));
    let rows = [Row {
        behind_said: row.age_said(NOW),
        ..row.clone()
    }];
    let next = next_change(&rows, NOW).expect("the age's next word");
    assert_eq!(next, NOW - UNWATCHED_AFTER_S - 600 + 2 * 3_600);
    assert_eq!(row.age_said(next).as_deref(), Some("2 h"));
    // Over every round that would be a record.
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
    // Negative controls: what its last look said.
    for (why, other) in [
        (
            "within the wake grace",
            Row {
                awake_since: NOW - 60,
                ..row.clone()
            },
        ),
        (
            "looked at within the bound",
            Row {
                looked_at: NOW - 60,
                watch_at: NOW - 60 + WATCH_GAP,
                ..row.clone()
            },
        ),
        (
            "the awake clock unknown",
            Row {
                awake_since: 0,
                ..row.clone()
            },
        ),
    ] {
        assert_eq!(other.unwatched(NOW), None, "{why}");
        assert_eq!(other.stall(NOW), healthy.stall(NOW), "{why}");
        assert_eq!(other.column(NOW), healthy.column(NOW), "{why}");
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
    // The owner's `--skip` holds it (and its record owes no watch at all).
    let skipped = Row {
        request: Request::Skip("2.1.282".to_string()),
        ..row.clone()
    };
    assert_eq!(skipped.stall(NOW), None);
    assert!(skipped.badge(NOW).is_none());
}

/// Over a real record and [`View`]: the watch is named and listed due, and with
/// no look stamping it the instant it turns `unwatched` ([`next_change`]) is
/// named next, so a live host that cannot watch wakes to mark it; at that
/// instant the row stalls `unwatched`.
///
/// Negative controls: a look stamped at the watch moves it a gap on; an unknown
/// awake clock names nothing; a wake a minute before holds it off for the
/// grace.
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
    // The awake clock unknown: no such instant, and nothing flagged.
    let (next, _, _) = look(0, due);
    assert_ne!(next, Some(turns));
    let (_, sent, _) = look(0, turns);
    assert_eq!(sent[0].stall(turns), None);
    // Woken a minute before: held off until the grace is over, and named.
    let woke = turns - 60;
    let (next, sent, _) = look(woke, turns);
    assert_eq!(sent[0].stall(turns), None, "inside the wake grace");
    assert_eq!(next, Some(woke + WAKE_GRACE_S), "the grace's end, named");
    // The healthy host: a look stamped at the watch moves it a gap on, and a
    // second past that the record is due again, not unwatched.
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

/// The host's watch starting, or resuming after sleep, is a wake too: the host
/// looks at the view before firing the watches it finds due, so marking by the
/// awake clock alone would flash `unwatched` at every start or resume. Over a
/// real record and [`View`]'s looks at chosen clocks ([`View::note_look`]): the
/// first look reads no `unwatched` and names the grace's end; past it, an
/// unstamped record is told; the first look after a sleep (wall clock ahead of
/// monotonic) or after the view stood down is a start again. Negative
/// controls: without looks noted it flashes at the first look; a host watching
/// all along is told at the same second.
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
    // Negative control: no look noted: the flash.
    let (_, row) = look(&mut View::default(), turns);
    assert_eq!(row.stalled.as_deref(), Some("unwatched"), "main flashes");

    // The host's first look: not missed yet, and the grace's end named.
    let mut view = View::default();
    view.note_look(t0, turns);
    let (next, row) = look(&mut view, turns);
    assert_eq!(row.stalled, None, "{row:?}");
    assert_eq!(row.stall(turns), None);
    assert_eq!(next, Some(turns + WAKE_GRACE_S), "the grace's end, named");
    // Past it, watching all along and never stamped: told.
    let later = turns + WAKE_GRACE_S;
    view.note_look(after(WAKE_GRACE_S), later);
    let (_, row) = look(&mut view, later);
    assert_eq!(row.stalled.as_deref(), Some("unwatched"), "told past it");
    // The first look after a sleep of eight hours: a start again.
    let resumed = later + 8 * 3_600;
    view.note_look(after(WAKE_GRACE_S + 60), resumed);
    let (_, row) = look(&mut view, resumed);
    assert_eq!(row.unwatched(resumed), None, "the first look after a sleep");
    assert_eq!(row.stalled.as_deref(), Some("overdue"), "its own stall");
    // Negative control: awake all along, clocks together: told.
    let mut awake = View::default();
    awake.note_look(t0, turns - 8 * 3_600);
    awake.note_look(after(8 * 3_600), turns);
    let (_, row) = look(&mut awake, turns);
    assert_eq!(
        row.stalled.as_deref(),
        Some("unwatched"),
        "no sleep between"
    );
    // Stood down (the switch off an hour), and back on: a start again.
    view.stand_down(&mut |_| {});
    let back = resumed + 3_600;
    view.note_look(after(WAKE_GRACE_S + 60 + 3_600), back);
    let (_, row) = look(&mut view, back);
    assert_eq!(row.unwatched(back), None, "the first look back on");
    assert_eq!(row.stalled.as_deref(), Some("overdue"), "its own stall");
    pin_awake_since(0);
}

/// Two stand-in agents (this test binary, parked, in `tab`) run as JOBS of a
/// job-control zsh that leads a pseudo-terminal of its own: one started in
/// the background, as a person's `^Z` leaves an agent — its own process
/// group, not the terminal's foreground — and one the zsh became, the
/// terminal's foreground. `(front, back pid, the pty's (master, slave))`:
/// the caller kills both (`SIGKILL`), closes the pty and reaps the front.
///
/// The test holds the SLAVE open for the jobs' life, as their terminal's
/// other holder: both run with stdio on `/dev/null` (a parked test binary
/// writing to a terminal nobody reads would sit in its exit's drain), so once
/// both have exec'd no job holds the terminal, its last slave closes, and
/// the session loses it (`ps` reads no tty, `tpgid` none) — the front then
/// reads as aside too.
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
    // Both ends close-on-exec from birth: the zsh below holds its terminal,
    // never a copy of its master, and no child another test spawns holds either.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned here
    // alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let exe = std::env::current_exe().expect("exe");
    let park = format!(
        "'{}' harness::upgrade_drive::tests::park_when_asked --exact </dev/null >/dev/null 2>&1",
        exe.display()
    );
    let pid_file = dir.join("back.pid");
    // `std`'s, for the hook below, with the strip installed first: the zsh
    // and the jobs it starts hold their stdio and nothing else of this process.
    let mut zsh = std::process::Command::new("/bin/zsh");
    zsh.inherit_nothing()
        .args([
            "-f",
            "-c",
            &format!(
                "setopt monitor; {park} & print -r -- $! > '{}'; exec {park}",
                pid_file.display()
            ),
        ])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    // SAFETY: async-signal-safe calls only, between fork and exec: a new
    // session, and the terminal on stdin made its controlling terminal.
    unsafe {
        zsh.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut front = zsh.spawn().expect("zsh");
    // Exec'd, not a fork of the zsh still reading as it.
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

/// A tab whose agent is not in front (suspended with `^Z`) is never told
/// "aterm's own fault": no worker runs for it, so nothing stamps the record.
/// Over real processes on a real pty and real session files, the holder is
/// proven in its tab and read not in front by both the window's roster and the
/// CLI's kernel read; its row reads `not-foreground`, never `unwatched`.
/// Negative control: the job in front with the same record reads `unwatched`.
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
    // Behind seven hours, nothing having looked for three.
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
    let in_window = holders(&o.home, &sessions, Some(&tabs));
    let by_cli = holders(&o.home, &sessions, None);
    let (cli, vetted) = status_rows(&o, true);
    let mut view = View::default();
    let mut sent: Vec<Row> = Vec::new();
    let _ = view.refresh_with(
        &o,
        &tabs,
        now,
        &|s| holders(&o.home, s, Some(&tabs)),
        &mut |rows| sent = rows.to_vec(),
    );
    // SAFETY: `kill` sends one signal to the pid this test's jobs reported;
    // SIGKILL, never a core-dump signal (AGENTS.md).
    unsafe {
        libc::kill(libc::pid_t::try_from(back).expect("pid"), libc::SIGKILL);
    }
    let _ = front.kill();
    // The terminal hung up before the reap: an exit never waits on a drain.
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
        // A record at first; one digest line once it has stood an hour
        // ([`SUSPENDED_AFTER_S`]): the owner's own setup, never aterm's fault.
        let stood = Row {
            wait_since: now - SUSPENDED_AFTER_S - 60,
            ..aside.clone()
        };
        let Attention::Digest(need) = stood.attention(now) else {
            panic!("{how}: one digest line: {:?}", stood.attention(now));
        };
        assert_eq!(need.kind, "suspended", "{how}");
        let cause = aside.overdue_cause(now);
        assert_eq!(cause, "its agent is not in front in its tab", "{how}");
        assert!(!cause.contains("fault"), "{how}: {cause}");
        let words = aside.stall_words(now).expect("stalled");
        assert!(
            words.ends_with("it is not in front in its tab"),
            "{how}: {words}"
        );
        assert_eq!(aside.remedy(now), Some(Remedy::Waits), "{how}");
        assert!(
            !aside.asks_on_its_own(now),
            "{how}: a row, the person's to mend"
        );
        let line = aside.line(now);
        assert!(
            line.contains(" wait=not-foreground wait_for=- "),
            "{how}: {line}"
        );
        assert!(line.contains(" cause=person "), "{how}: {line}");
    }
}

/// A tab another supervisor's claim holds is never told "aterm's own fault":
/// the host runs no worker behind a live claim. Told the claimed tabs
/// ([`View::set_claimed`]), a real [`View`] reads the move as held by that
/// claim and never `unwatched`; stood down, it forgets them. `--status` reads
/// the claim from the instance ([`status_rows`]) by the same rule
/// ([`Row::set_aside`]). Negative controls: no claim, or another tab's, reads
/// `unwatched`; a conversation resumed outside aterm is its own
/// ([`Row::outside`]).
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
    let cause = row.overdue_cause(NOW);
    assert_eq!(cause, "another supervisor holds its tab");
    assert!(!cause.contains("fault"), "{cause}");
    let words = row.stall_words(NOW).expect("stalled");
    assert!(
        words.ends_with("another supervisor holds its tab"),
        "{words}"
    );
    assert!(
        row.waiting_line("09:00")
            .contains("it moves once that supervisor lets the tab go"),
        "{}",
        row.waiting_line("09:00")
    );
    // Stood down, the claims are forgotten with everything else: the next
    // look, told none, reads the record as it is.
    view.stand_down(&mut |_| {});
    let mut sent: Vec<Row> = Vec::new();
    let _ = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent = rows.to_vec());
    assert_eq!(
        sent[0].stall(NOW).as_deref(),
        Some("unwatched"),
        "forgotten"
    );
    // `--status`, the claims the instance names.
    let cli = |claimed: &[&str], held: &dyn Fn(&BTreeSet<String>) -> Option<Vec<Holder>>| {
        let claimed: BTreeSet<String> = claimed.iter().map(|t| (*t).to_string()).collect();
        let (mut rows, vetted) = status_rows_with(&o, NOW, held, &|| claimed.clone());
        assert!(vetted);
        rows.remove(0)
    };
    let by_cli = cli(&[TAB], &held);
    let facts = |r: &Row| {
        (
            r.aside.clone(),
            r.wait.clone(),
            r.wait_since,
            r.stall(NOW),
            r.cause_word(NOW),
            r.told_word(NOW),
        )
    };
    assert_eq!(facts(&by_cli), facts(&row), "the window's facts");
    assert_eq!(by_cli.unwatched(NOW), None);
    let line = by_cli.line(NOW);
    for field in [
        " wait=claimed wait_for=- ",
        " stalled=overdue ",
        " cause=person ",
    ] {
        assert!(line.contains(field), "{field}: {line}");
    }
    for claimed in [&[][..], &["s-another"][..]] {
        assert_eq!(
            cli(claimed, &held).stall(NOW).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {claimed:?}"
        );
    }
    let outside = cli(&[TAB], &|sessions: &BTreeSet<String>| {
        Some(
            sessions
                .iter()
                .map(|s| holder(s, "2.1.281", None))
                .collect(),
        )
    });
    assert!(outside.outside && outside.aside.is_empty(), "{outside:?}");
    assert_eq!(outside.stall(NOW).as_deref(), Some("overdue"));
    pin_awake_since(0);
}

/// A RECORD NO BUILD GAVE A TAB READS THE CLAIM ON ITS HOLDER'S TAB: `--status`
/// shows such a row under the tab its holder is proven in, and reads its aside
/// only once that tab is filled in, so a claim there holds it (never
/// `stalled=unwatched`). NEGATIVE CONTROLS: the same record with no tab
/// claimed, or another tab, reads `unwatched`.
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
        let (mut rows, vetted) = status_rows_with(&o, NOW, &behind_in(TAB), &|| claimed.clone());
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

/// THE ROW A WORD PRINTS IS READ AS `--status` READS IT: `aterm harness upgrade
/// <sid> --now` on a tab another supervisor's claim holds prints the claim,
/// never `stalled=unwatched` ([`Row::set_aside`]). NEGATIVE CONTROLS: no tab
/// claimed, or another tab, and the same word's row reads `unwatched`.
#[test]
fn the_row_a_word_prints_reads_the_claim_on_its_tab() {
    let dir = ScratchDir::new("upgrade-status", "ask-claimed");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let st = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now - 7 * 3_600,
        looked_at: now - 3 * 3_600,
        ..St::default()
    };
    pin_awake_since(now - 86_400);
    let hurry = |claimed: &[&str]| {
        super::super::save_at(&o, "aaa", &st, now - 7 * 3_600);
        let claimed: BTreeSet<String> = claimed.iter().map(|t| (*t).to_string()).collect();
        ask_aside(&o, TAB, Ask::Now, &|| claimed.clone()).expect("the word lands")
    };
    let row = hurry(&[TAB]);
    assert_eq!((row.aside.as_str(), row.wait.as_str()), (CLAIMED, CLAIMED));
    assert_eq!(row.unwatched(now_s()), None);
    let line = row.line(now_s());
    assert!(
        line.contains(" wait=claimed ") && !line.contains("unwatched"),
        "{line}"
    );
    for claimed in [&[][..], &["s-another"][..]] {
        assert_eq!(
            hurry(claimed).stall(now_s()).as_deref(),
            Some("unwatched"),
            "NEGATIVE CONTROL, {claimed:?}"
        );
    }
    pin_awake_since(0);
}

/// A RECORD NO BUILD GAVE A TAB IS WATCHED IN THE TAB ITS HOLDER IS PROVEN IN:
/// an older build recorded the tab only at the announcement. It is named due,
/// and its instant named, under the live tab its holder is proven in, as
/// `--status` shows it. NEGATIVE CONTROLS: the same record whose holder
/// is proven in a tab this instance does not show is neither listed nor
/// timed; and with its holders unread nothing proves its tab, so nothing is
/// watched off a guess.
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

/// The moment a person stops being named is a change the view names: the row
/// carries [`Row::person_named`] and [`next_change`] names the instant it flips,
/// since the host only looks at named instants. Negative controls: past it
/// nothing is to come; a Codex tab's attended wait and other waits name none;
/// the row before the flip is unchanged.
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
    // (A row busy with its own work names the digest's day, not a flip.)
    assert_eq!(
        next_change(&[pending(7 * 3_600)], NOW),
        Some(NOW - 7 * 3_600 + DIGEST_AFTER_S)
    );

    // The row handed over changes at the flip, and by that alone.
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

/// The host hands the window rows only when they change, and crossing
/// [`STALLED_AFTER_S`] is a change; a tab no longer on the roster is left out;
/// standing down sends one empty summary.
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
    // An unreadable roster says nothing, and asks for a look again on a
    // growing pause (the control socket is bound after the host starts).
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
    // Crossing the stall bound is news, though nothing in the file moved.
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

/// Upgrade records that cannot be read whole say nothing (as an unreadable
/// roster or session files do), so a successor's look does not withdraw stall
/// rows. An unlistable state directory or an unreadable listed record hands
/// over nothing; `--status` shows what reads. No directory at all is a look.
#[test]
fn records_that_cannot_be_read_whole_hand_the_window_nothing() {
    let dir = ScratchDir::new("upgrade-status", "unreadable-records");
    let o = opts(&dir);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    // Not listable: a file where the directory goes (ENOTDIR).
    std::fs::create_dir_all(&o.state).expect("state");
    std::fs::write(state_dir(&o), b"").expect("a file in the directory's place");
    let next = view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert!(
        sent.is_empty(),
        "an unlistable state directory hands over nothing"
    );
    assert_eq!(next, None);
    assert!(rows_at(&o, NOW).is_empty(), "--status reads no records");
    // Listed, not readable: a record that loops on itself (ELOOP), beside
    // a stalled record that reads.
    std::fs::remove_file(state_dir(&o)).expect("unblock");
    std::fs::create_dir_all(state_dir(&o)).expect("state dir");
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
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert!(
        sent.is_empty(),
        "a record that cannot be read hands over nothing"
    );
    assert_eq!(
        rows_at(&o, NOW).len(),
        1,
        "--status reads the one that reads"
    );
    // Read whole again: the look is handed over.
    std::fs::remove_file(&looped).expect("unloop");
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].len(), 1);
    assert_eq!(sent[0][0].stalled.as_deref(), Some("overdue"));
    // No directory at all: nothing recorded, and that is a look.
    std::fs::remove_dir_all(state_dir(&o)).expect("no records");
    view.refresh_with(&o, &tabs, NOW, &held, &mut |rows| sent.push(rows.to_vec()));
    assert_eq!(sent.len(), 2);
    assert!(sent[1].is_empty(), "no directory: no records, handed over");
}

/// ONE STATE FILE, OLDER WRITERS: a file written by the build before the
/// owner's view, by a build that wrote fields this one does not read
/// (`told`, `ready`, the retired board's `yielded`/`yielded_until`) and by the
/// owner's-view build (`request`, `wait`, `pending_since`…) is read by this
/// one, each missing field at its default (no `pending_since`: behind since
/// its salt) and an unknown one ignored — and a field this build writes that
/// another reads as unknown changes nothing it reads. This build's own fields
/// survive the file, and a request word it cannot read asks for nothing.
#[test]
fn a_state_file_from_either_older_writer_is_read_with_its_missing_fields_at_default() {
    // The build before the owner's view: none of its fields.
    let before = r#"{"phase":"announced","why":"","from":"2.1.281","to":"2.1.282",
        "source":"managed","marker":"ATERM-UPGRADE-READY-1","notice_start":"x","tab":"s-1",
        "line":"","noted":"","model_before":"","launch_model":"","resumed_on":"",
        "at":90,"asks":2,"salt":7,"last_seq":3,"seq_since":80,"pid":11,"notice_pid":11,
        "shell":10,"mark":0,"confirm_by":0,"resumed_pid":0}"#;
    let st = St::from_json(before).expect("the older build's file");
    assert_eq!(st.phase, Phase::Announced { at_s: 90, asks: 2 });
    assert_eq!((st.request.clone(), st.wait.as_str()), (Request::None, ""));
    assert_eq!(st.behind_since(), 7, "no pending_since: the salt");
    // A build that wrote `told` and `ready`: ignored, nothing of the owner's.
    let unknown = before.replace(r#""asks":2,"#, r#""asks":2,"told":1,"ready":1,"#);
    let st = St::from_json(&unknown).expect("an unknown field's file");
    assert_eq!(st.phase, Phase::Announced { at_s: 90, asks: 2 });
    assert_eq!(st.request, Request::None);
    // The owner's-view build: its fields.
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
    // Retired `yielded` and `yielded_until` fields: ignored.
    let board = owners.replace(
        r#""asks":2,"#,
        r#""asks":2,"yielded":1,"yielded_until":270,"#,
    );
    let st = St::from_json(&board).expect("a board-era file");
    assert_eq!(
        Row::of("c", &st, 100),
        Row::of("c", &St::from_json(&owners).unwrap(), 100)
    );
    // And this build's own file carries all of it, back to itself.
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

/// A control socket at `<dir>/t.sock` (token beside it) answering `OK` to every
/// request and recording them, standing in for the window's instance. The
/// accept loop ends with the test process.
#[cfg(unix)]
fn recording_socket(
    dir: &std::path::Path,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{BufRead as _, Write as _};
    const TOKEN: &str = "0badf00d0badf00d";
    let sock = dir.join("t.sock");
    std::fs::write(dir.join("t.sock.token"), format!("{TOKEN}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let Ok(mut out) = conn.try_clone() else {
                continue;
            };
            let mut lines = std::io::BufReader::new(conn).lines();
            if !lines
                .next()
                .is_some_and(|l| l.is_ok_and(|l| l == format!("AUTH {TOKEN}")))
            {
                continue;
            }
            for line in lines {
                let Ok(line) = line else { break };
                if let Ok(mut log) = log.lock() {
                    log.push(line);
                }
                if writeln!(out, "OK").is_err() {
                    break;
                }
            }
        }
    });
    (sock.to_string_lossy().into_owned(), asked)
}

/// The tab's mark: the host raises `meta set attention owner=upgrade` on a
/// stalled tab once and lowers it when the stall ends. Negative control: a tab
/// merely waiting for its turn end is never marked.
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
    // THE ONE STALL THAT MARKS THE TAB: the upgrade ended the agent and it is
    // not back (the owner is needed now, and the action is theirs alone). A
    // refusal, a round that gave up and an overdue wait are none: the log, or
    // the digest (`an_overdue_tab_is_never_marked`, `attention_is_decided_by_
    // the_owners_action_never_by_age`).
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
    // The agent the upgrade ended is gone: no process holds its conversation.
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let sweep = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    };
    let sweep_after_exit = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    };
    sweep(&mut view);
    // THE FIRST LOOK OF A PROCESS LOWERS WHAT AN EARLIER ONE RAISED (a
    // seamless update carries every keyed attention across the handoff, and
    // this view starts with nothing marked): one idempotent
    // `unset` for the tab it wants no mark on, and nothing that marks.
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
        marked[0].contains("could not start it again")
            && marked[0].contains("Resume it in that tab with this command: claude --resume aaa"),
        "the mark says why the owner and the one action: {marked:?}"
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
}

/// A MARK THIS VIEW DID NOT RAISE IS LOWERED AT ITS FIRST LOOK: a seamless
/// update carries keyed attention into the successor, whose view begins with
/// nothing raised, so the successor's first whole look lowers the
/// `owner=upgrade` entry of every live tab it does not want marked, once, by
/// that key alone. NEGATIVE CONTROLS: its later looks send
/// nothing more; a tab it wants marked is written, never lowered; and a view
/// that stood down settles its tabs again at its next look.
#[test]
fn a_carried_upgrade_mark_is_lowered_by_the_successors_first_look() {
    let dir = ScratchDir::new("upgrade-status", "mark-carried");
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
    // The one stall that marks a tab: the agent the upgrade ended is not
    // back (`only_a_stalled_tab_is_marked_and_its_mark_is_lowered_when_it_
    // recovers`), no process holding its conversation.
    let stalled = St {
        phase: Phase::Failed("stale-exit".to_string()),
        exited_at: now_s() - 600,
        ..waiting.clone()
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let sweep = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    };
    let sweep_after_exit = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    };
    let taken = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|mut a| {
                std::mem::take(&mut *a)
                    .into_iter()
                    .filter(|l| l.contains(" meta "))
                    .collect()
            })
            .unwrap_or_default()
    };
    let lowered = format!("@{TAB} meta unset attention owner=upgrade");
    // The producer's view raises the mark, and the update carries it.
    save(&o, "aaa", &stalled);
    let mut producer = View::default();
    sweep_after_exit(&mut producer);
    assert!(
        taken(&asked)
            .iter()
            .any(|l| l.starts_with(&format!("@{TAB} meta set attention owner=upgrade"))),
        "raised"
    );
    // The stall is over in the successor; its fresh view lowers the mark.
    save(&o, "aaa", &waiting);
    let mut successor = View::default();
    sweep(&mut successor);
    assert_eq!(
        taken(&asked),
        std::slice::from_ref(&lowered),
        "lowered at the first look"
    );
    sweep(&mut successor);
    assert!(taken(&asked).is_empty(), "once");
    // NEGATIVE CONTROL: a successor that still wants it marked writes it.
    save(&o, "aaa", &stalled);
    let mut wanting = View::default();
    sweep_after_exit(&mut wanting);
    let said = taken(&asked);
    assert!(!said.contains(&lowered), "{said:?}");
    assert!(
        said.iter()
            .any(|l| l.starts_with(&format!("@{TAB} meta set attention owner=upgrade"))),
        "{said:?}"
    );
    // A view that stood down settles its tabs again at its next look.
    save(&o, "aaa", &waiting);
    successor.stand_down(&mut |_| {});
    let _ = taken(&asked);
    sweep(&mut successor);
    assert_eq!(taken(&asked), [lowered]);
}

/// WHAT A ROW SAYS IT MOVES: a build upgrade names both builds (and the
/// model once the announcement named one); a same-build restart for a model
/// move names the model, never `2.1.282 → 2.1.282`.
#[test]
fn a_row_names_the_move_a_model_restart_included() {
    let row = pending(60);
    assert_eq!(row.move_words(), "Claude Code 2.1.281 → 2.1.282");
    let with = Row {
        model: "claude-opus-5-5".to_string(),
        ..row.clone()
    };
    assert_eq!(
        with.move_words(),
        "Claude Code 2.1.281 → 2.1.282 with claude-opus-5-5"
    );
    let same = Row {
        from: "2.1.282".to_string(),
        ..row.clone()
    };
    assert_eq!(same.move_words(), "Claude Code 2.1.282 → another model");
    let named = Row {
        model: "claude-opus-5-5".to_string(),
        ..same
    };
    assert_eq!(named.move_words(), "Claude Code 2.1.282 → claude-opus-5-5");
}

/// What is shown is vetted against live processes: a pending, announced or
/// stopped upgrade stands only while a live process holds its conversation in
/// its tab on a build older than its target; a finished move and a restart in
/// flight always stand. The host counts no holder whose tab it cannot prove;
/// the CLI, with no roster, disproves none.
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
    // A same-build restart (a model move) waits on a holder still on its
    // build, and not on one past it or in another tab.
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

/// AN OVERDUE TAB IS NEVER MARKED (the owner: "don't continue to persist
/// alerts to the user that are not actionable"), and THE TAB'S MARK OF A STALL
/// THAT IS ONE IS SENT ONCE: a mark text carrying an age would be re-sent at
/// every sweep, each a meta-change event and a wake. Three sweeps a minute
/// apart on an overdue tab send NONE;
/// when the agent it was moving is gone (the upgrade ended it and it is not
/// back) the mark is sent ONCE, holding no age; a change of KIND is a new
/// text, sent once more.
#[cfg(unix)]
#[test]
fn an_overdue_tab_is_never_marked() {
    let dir = ScratchDir::new("upgrade-status", "mark-once");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let overdue = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now - 7 * 3_600 - 60,
        wait: "not-idle:busy".to_string(),
        wait_since: now - 7 * 3_600,
        ..St::default()
    };
    save(&o, "aaa", &overdue);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let sets = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter(|l| l.contains(" meta set attention "))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let held = behind_in(TAB);
    let mut view = View::default();
    for minute in 0..3 {
        view.refresh_with(&o, &tabs, now + minute * 60, &held, &mut |_| {});
    }
    // The wait moving under it (busy, then a background shell) is not a new
    // stall either.
    save(
        &o,
        "aaa",
        &St {
            wait: "not-idle:shell".to_string(),
            ..overdue.clone()
        },
    );
    view.refresh_with(&o, &tabs, now + 180, &held, &mut |_| {});
    assert!(
        sets(&asked).is_empty(),
        "behind for hours is no reason to mark a tab: {:?}",
        sets(&asked)
    );
    // The upgrade ended the agent and it is not back: marked once, whatever
    // the number of looks, and a refusal (a digest line) marks nothing.
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("not-a-shell-job".to_string()),
            ..overdue.clone()
        },
    );
    view.refresh_with(&o, &tabs, now + 240, &held, &mut |_| {});
    assert!(sets(&asked).is_empty(), "a refusal is the digest's");
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("stale-exit".to_string()),
            exited_at: now,
            ..overdue
        },
    );
    // (The agent it ended is gone: no process holds its conversation.)
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    view.refresh_with(&o, &tabs, now + 300, &nobody, &mut |_| {});
    view.refresh_with(&o, &tabs, now + 360, &nobody, &mut |_| {});
    let marked = sets(&asked);
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(
        marked[0].contains("could not start it again")
            && marked[0].contains("Resume it in that tab with this command: claude --resume aaa"),
        "{marked:?}"
    );
}

/// The user's `--now` ends an overdue stall: under `--now` it is moving (no
/// stall, remedy or mark). An overdue stall's remedy names `--now` only where
/// `--now` moves it; waits it cannot end (the READY answer, a draft, a box, a
/// hold, work under the agent, or a non-idle status other than a turn) name
/// [`Remedy::Waits`]. Negative controls: without the word it is overdue, and a
/// stop is still a stall under `--now`.
#[test]
fn an_owners_now_ends_an_overdue_stall_and_its_remedy_is_the_waits() {
    let overdue = pending(7 * 3_600);
    assert_eq!(overdue.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(
        overdue.badge(NOW),
        None,
        "behind is no reason to mark a tab"
    );
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 60,
        ..overdue.clone()
    };
    assert_eq!(hurried.stall(NOW), None, "the owner has acted");
    assert_eq!(hurried.remedy(NOW), None);
    assert_eq!(hurried.badge(NOW), None, "the mark is lowered");
    assert!(hurried.moving(NOW), "counted as moving");
    assert!(
        hurried
            .column(NOW)
            .starts_with("pending/2.1.282/not-idle:busy/"),
        "{}",
        hurried.column(NOW)
    );
    assert!(
        hurried
            .line(NOW)
            .contains(" request=now next_round=- stalled=- held_by=- release=- "),
        "{}",
        hurried.line(NOW)
    );
    let stopped = Row {
        phase: Phase::Failed("argv:--print".into()),
        ..hurried
    };
    assert!(stopped.stall(NOW).is_some(), "a stop is still a stall");
    // What `--now` still moves once overdue: an overdue upgrade already stands
    // on the ladder's last rung, which `--now` is, so the settle, a nearby
    // person and a running turn are not moved by it. Its own text left typed
    // and a notice behind a full queue still are.
    for (wait, remedy) in [
        ("", Remedy::Waits),
        ("settling", Remedy::Waits),
        ("attended", Remedy::Waits),
        ("busy", Remedy::Waits),
        ("not-idle:busy", Remedy::Waits),
        ("daemon-turn", Remedy::Waits),
        ("left-typed-backoff", Remedy::Now),
        ("queued", Remedy::Now),
        ("not-idle:shell", Remedy::Waits),
        ("status-stale:busy", Remedy::Waits),
        ("status-stale:shell", Remedy::Waits),
        ("not-idle:waiting", Remedy::Waits),
        ("not-idle", Remedy::Waits),
        ("awaiting-ready", Remedy::Waits),
        ("draft", Remedy::Waits),
        ("box", Remedy::Waits),
        ("held", Remedy::Waits),
        ("background", Remedy::Waits),
    ] {
        let r = Row {
            wait: wait.to_string(),
            ..overdue.clone()
        };
        assert_eq!(r.remedy(NOW), Some(remedy), "waiting {wait:?}");
    }
}

/// Every word from the user arms a new round: each word mints a new salt past
/// every marker the old one could make, so a READY given before the word never
/// answers a notice typed after it. Time behind is kept. Negative control: a
/// step's own state keeps its round.
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
    ask(&o, TAB, Ask::Defer(600), false).expect("defer");
    let held = load(&o, "aaa").expect("state");
    assert_eq!(held.phase, Phase::Pending, "the hold ends the notice");
    assert!(held.marker.is_empty(), "and its marker");
    assert!(held.markers.is_empty(), "every marker of its round");
    // The agent asked to wind down is released: one line is owed, typed at the
    // next step that may type.
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
    assert!(held.request_at >= now_s() - 5, "when the word was given");
    // `--now` over a pending upgrade: a round of its own too.
    ask(&o, TAB, Ask::Now, false).expect("--now");
    let hurried = load(&o, "aaa").expect("state");
    assert!(hurried.salt > held.salt, "the owner's word opens a round");
    // Negative control: a step's own state for the same target keeps it.
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

/// An ineffective `--now` cannot hide a stall for good: the word quiets the
/// stall for [`NOW_QUIETS_S`] from when it was given; unmoved by then it reads
/// `overdue` again with the remedy of what it is stuck on. Negative control:
/// inside that window it stays quiet.
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
    assert!(shell.moving(soon));
    assert_eq!(shell.badge(soon), None, "the mark is lowered");
    let later = NOW + NOW_QUIETS_S;
    assert_eq!(shell.stall(later).as_deref(), Some("overdue"));
    assert_eq!(shell.remedy(later), Some(Remedy::Waits));
    assert_eq!(shell.badge(later), None, "never a mark: the column says it");
    let month = NOW + 30 * 86_400;
    assert_eq!(shell.stall(month).as_deref(), Some("overdue"));
    // A word an older build wrote carries no time: it quiets nothing.
    let untimed = Row {
        request_at: 0,
        ..shell
    };
    assert_eq!(untimed.stall(NOW).as_deref(), Some("overdue"));
}

/// A GAVE-UP UPGRADE IS NOT STALLED, BEFORE OR AFTER `--now` RE-ARMS IT (the
/// owner: "you should NEVER have upgrades stalled"). A round that gave up
/// rests until its next one, which `--status` names (`next_round=`), and is
/// never marked; the owner's `--now` re-arms it at once, as its remedy says,
/// and nothing is marked then either. NEGATIVE CONTROL: the same row as a
/// refusal (`not-a-shell-job`, which a new round meets again) is a stall, a
/// line in the digest, but no mark.
#[cfg(unix)]
#[test]
fn a_give_up_rests_unmarked_and_now_re_arms_it_at_once() {
    let dir = ScratchDir::new("upgrade-status", "re-armed");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    let gave_up = St {
        phase: Phase::Failed("unanswered".to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: now - 5 * 3_600,
        failed_at: now - 60,
        ..St::default()
    };
    save(&o, "aaa", &gave_up);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut view = View::default();
    let mut last = Vec::new();
    view.refresh_with(&o, &tabs, now, &held, &mut |rows| last = rows.to_vec());
    assert_eq!(last[0].stall(now), None, "resting, not stalled");
    assert_eq!(last[0].retry_at, now - 60 + upgrade::RETRY_S);
    assert_eq!(
        last[0].next_round_in(now),
        Some(upgrade::RETRY_S - 60),
        "{:?}",
        last[0]
    );
    assert!(
        last[0]
            .line(now)
            .contains(" next_round=1h59m stalled=- held_by=- release=- "),
        "{}",
        last[0].line(now)
    );
    assert_eq!(last[0].column(now), "pending/2.1.282/next-round:1h59m/5h");
    assert_eq!(
        last[0].remedy(now),
        Some(Remedy::AskAgain),
        "--now asks at once"
    );
    let row = ask(&o, TAB, Ask::Now, false).expect("--now re-arms a give-up");
    assert_eq!(row.phase, Phase::Pending);
    assert_eq!(row.stall(now), None, "{row:?}");
    view.refresh_with(&o, &tabs, now + 60, &held, &mut |rows| last = rows.to_vec());
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].stall(now + 60), None, "not stalled");
    let meta = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter(|l| l.contains(" meta set "))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    // Marks SET, never the view's first-look lowering of one it did not
    // raise.
    let sets = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        meta(asked)
            .into_iter()
            .filter(|l| l.contains(" meta set "))
            .collect()
    };
    assert!(sets(&asked).is_empty(), "never marked: {:?}", meta(&asked));
    // NEGATIVE CONTROL: a refusal a new round meets again is a stall.
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("not-a-shell-job".to_string()),
            ..gave_up
        },
    );
    view.refresh_with(&o, &tabs, now + 120, &held, &mut |rows| {
        last = rows.to_vec()
    });
    assert_eq!(
        last[0].stall(now + 120).as_deref(),
        Some("refused:not-a-shell-job")
    );
    assert!(
        meta(&asked).is_empty(),
        "a refusal is the digest's, never a mark: {:?}",
        meta(&asked)
    );
}

/// A `line:` refusal is the relaunch line's, said as its own cause, not "launch
/// flags cannot be carried". Negative control: an `argv:` refusal is still the
/// flags'.
#[test]
fn a_line_refusal_says_the_line_not_the_flags() {
    let refused = |why: &str| Row {
        phase: Phase::Failed(why.to_string()),
        ..pending(60)
    };
    let fish = refused("line:fish: the agent's directory differs from the shell's");
    let words = fish.stall_words(NOW).expect("stalled");
    assert!(words.contains("fish"), "{words}");
    assert!(words.contains("directory"), "{words}");
    assert!(!words.contains("flags"), "{words}");
    assert_eq!(
        fish.badge(NOW),
        None,
        "a refusal is a digest line, not a mark"
    );
    assert!(
        matches!(fish.attention(NOW), Attention::Digest(n) if n.kind == "refused-launch"),
        "{:?}",
        fish.attention(NOW)
    );
    for why in [
        "line:the line would be 1100 bytes (max 1000)",
        "line:a control character in the line",
    ] {
        let words = refused(why).stall_words(NOW).expect("stalled");
        assert!(words.contains("line"), "{why}: {words}");
        assert!(!words.contains("flags"), "{why}: {words}");
    }
    assert_eq!(
        refused("argv:unknown-flag --frob")
            .stall_words(NOW)
            .as_deref(),
        Some("its launch flags cannot be carried into a resume")
    );
    assert_eq!(refused("line:x").remedy(NOW), Some(Remedy::ByHand));
}

/// The stand-in agent: this test binary parked in `harness::upgrade_drive`'s
/// park test, its tab in its environment. Not `/bin/sleep`: a platform
/// binary's environment is hidden from `KERN_PROCARGS2`.
#[cfg(unix)]
fn parked_agent(tab: &str) -> aterm_uds::spawnfd::Child {
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("exe"))
        .args(["harness::upgrade_drive::tests::park_when_asked", "--exact"])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in agent");
    // Its environment is read once it has exec'd (a fork not yet exec'd reads
    // as this process, with this process's environment).
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

/// Claude's session file for `pid`, holding `session` on `version`, with the
/// kernel's start time.
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

/// Negative control through the real reads: a `Pending` seven hours old in a
/// live tab, whose live process's session file reports the target build
/// (moved by hand), hands the window no row and marks no tab. The same process
/// on the older build is one row and one mark; once the conversation ends,
/// both go.
#[cfg(unix)]
#[test]
fn a_state_whose_session_is_current_or_gone_shows_nothing() {
    let dir = ScratchDir::new("upgrade-status", "phantom");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let mut agent = parked_agent(TAB);
    let session = "0badf00d-1111-2222-3333-444455556666";
    save(
        &o,
        session,
        &St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 7 * 3_600,
            wait: "not-idle:busy".to_string(),
            wait_since: now_s() - 7 * 3_600,
            ..St::default()
        },
    );
    // The roster proves the tab by the agent's group; its environment agrees.
    let group = super::super::process_group(agent.id()).expect("group");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(group),
    }];
    let meta = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter(|l| l.contains(" meta set "))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    // Marks SET, never the view's first-look lowering of one it did not
    // raise.
    let sets = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        meta(asked)
            .into_iter()
            .filter(|l| l.contains(" meta set "))
            .collect()
    };
    let mut view = View::default();
    let mut sent: Vec<Vec<Row>> = Vec::new();

    register_as(&o.home, agent.id(), session, "2.1.282");
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|sessions| holders(&o.home, sessions, Some(&tabs)),
        &mut |rows| sent.push(rows.to_vec()),
    );
    let current = (sent.clone(), sets(&asked));

    register_as(&o.home, agent.id(), session, "2.1.281");
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|sessions| holders(&o.home, sessions, Some(&tabs)),
        &mut |rows| sent.push(rows.to_vec()),
    );
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|sessions| holders(&o.home, sessions, Some(&tabs)),
        &mut |rows| sent.push(rows.to_vec()),
    );
    let behind = (sent.clone(), sets(&asked));

    let _ = agent.kill();
    let _ = agent.wait();
    let _ = view.refresh_with(
        &o,
        &tabs,
        now_s(),
        &|sessions| holders(&o.home, sessions, Some(&tabs)),
        &mut |rows| sent.push(rows.to_vec()),
    );
    let gone = (sent.clone(), meta(&asked));

    assert_eq!(
        current.0,
        vec![Vec::<Row>::new()],
        "a session on its target hands over no row"
    );
    assert!(current.1.is_empty(), "and marks nothing: {:?}", current.1);
    assert_eq!(behind.0.len(), 2, "{:?}", behind.0);
    assert_eq!(behind.0[1].len(), 1);
    assert_eq!(behind.0[1][0].stalled.as_deref(), Some("overdue"));
    // Behind for hours is no reason to mark a tab: the row is handed over
    // (the `upgrade=` column, `--status`), and nothing more.
    assert!(behind.1.is_empty(), "no mark: {:?}", behind.1);
    assert_eq!(gone.0.len(), 3);
    assert!(
        gone.0[2].is_empty(),
        "an ended conversation hands over none"
    );
    assert!(
        gone.1.is_empty(),
        "and nothing was ever marked: {:?}",
        gone.1
    );
}

/// A state an older build wrote has no tab; it is shown under the tab its live
/// holder is proven in, and the user's word for that tab lands and records the
/// tab. Negative control: a holder proven in another tab is neither listed nor
/// taken by a word for this one.
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
    // As written: the one writer stamps its progress (`St::progress_at`).
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

/// Claude's session file for `pid` as [`register_as`] writes it, its status
/// `status` since `since_s` (unix seconds).
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

/// A row no look has reached reads Claude's live status: a session minted
/// behind at attach, or whose last step typed its notice, records no wait
/// until a step looks again, and steps only come at idle screens or breaks.
/// The view reads the word the first look would record from the live holder's
/// status (`not-idle:<status>`), in `--status` and window rows alike: a box
/// reads `it asked a question and waits` with no `Upgrade now`, a background
/// shell `its own work runs`, each since the status says. A running turn
/// offers no `Upgrade now` either. Never over a recorded word. Negative
/// controls: an idle holder leaves the row as recorded; a recorded `settling`
/// stands.
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
    let announced = Phase::Announced {
        at_s: now - 3_600,
        asks: 1,
    };
    let cases = [
        (Phase::Pending, "", "waiting"),
        (announced, "", "waiting"),
        (Phase::Pending, "", "shell"),
        (Phase::Pending, "", "busy"),
        (Phase::Pending, "", "idle"),
        (Phase::Pending, "settling", "waiting"),
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
            &|sessions| holders(&o.home, sessions, Some(&tabs)),
            &mut |rows| sent.push(rows.to_vec()),
        );
        seen.push((vetted, cli, sent));
    }
    let _ = agent.kill();
    let _ = agent.wait();

    let want = [
        (
            "not-idle:waiting",
            Remedy::Waits,
            "it asked a question and waits",
            "2h",
        ),
        (
            "not-idle:waiting",
            Remedy::Waits,
            "it asked a question and waits",
            "2h",
        ),
        ("not-idle:shell", Remedy::Waits, "its own work runs", "2h"),
        // `--now` is the ladder's last rung, which a running turn, the settle
        // and nothing recorded do not pass.
        (
            "not-idle:busy",
            Remedy::Waits,
            "its turn is still running",
            "2h",
        ),
        ("", Remedy::Waits, "behind for 7 h", "-"),
        (
            "settling",
            Remedy::Waits,
            "waiting for the tab to settle",
            "10m",
        ),
    ];
    for (((phase, recorded, status), (vetted, cli, sent)), (wait, remedy, words, wait_for)) in
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
            let said = row.stall_words(now).expect("overdue");
            assert!(said.contains(words), "{case} ({surface}): {said}");
            assert!(!said.contains("not-idle"), "{case} ({surface}): {said}");
            let line = row.line(now);
            let dash = if wait.is_empty() { "-" } else { wait };
            assert!(
                line.contains(&format!(" wait={dash} wait_for={wait_for} ")),
                "{case} ({surface}): {line}"
            );
        }
    }
    // A box after the notice is a person's to answer: the upgrade waits under
    // it without re-asking or giving up, so it is a row with a mark.
    assert!(!seen[1].1[0].asks_on_its_own(now));
    assert!(!seen[3].1[0].asks_on_its_own(now), "a turn: no");
    assert_eq!(
        seen[0].1[0].column(now),
        "stalled/2.1.282/overdue:not-idle:waiting/7h"
    );
}

/// The window follows a row read off a live status: the view names the tabs
/// whose row follows their agent's live status ([`View::follows`]), the host
/// looks again whenever such a tab's screen moves, and the look sends the
/// changed row (box answered, a turn still running waits with no `Upgrade
/// now`). Negative control: a word a look recorded stands until the next step,
/// and no tab is followed for it.
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

    // A look while a box is up.
    record("");
    let next = view.refresh_with(&o, &tabs, now, &held("waiting", now - 600), &mut |rows| {
        sent.push(rows.to_vec());
    });
    let boxed = (next, view.follows().to_vec());
    // The one instant the look names is the record's own watch (rollout step
    // 7), which no status move changes.
    let watch = super::super::watch_at(&load(&o, session).expect("the record"));
    // The box answered: a turn runs. The host's look at the tab's move.
    let _ = view.refresh_with(&o, &tabs, now + 5, &held("busy", now + 1), &mut |rows| {
        sent.push(rows.to_vec());
    });
    let turn = view.follows().to_vec();
    // Negative control: a recorded word.
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

/// What the user can do, by kind of stall.
#[test]
fn each_kind_of_stall_names_the_remedy_that_moves_it() {
    let base = pending(60);
    assert_eq!(base.remedy(NOW), None, "not stalled: nothing to do");
    // Overdue on a running turn: already on the ladder's last rung, which
    // `--now` is, so it waits.
    assert_eq!(pending(7 * 3_600).remedy(NOW), Some(Remedy::Waits));
    assert_eq!(pending_with("queued").remedy(NOW), Some(Remedy::Now));
    // A wait no rung passes, blocking the move for a re-ask interval: blocked,
    // by hand, timed by its blocker's own clock whatever other wait the last
    // look found.
    let blocked = Row {
        wait: "daemon-turn".into(),
        wait_since: NOW - 60,
        blocked: "no-shell-integration".into(),
        blocked_since: NOW - BLOCKED_AFTER_S,
        ..base.clone()
    };
    assert_eq!(
        blocked.stall(NOW).as_deref(),
        Some("blocked:no-shell-integration")
    );
    assert_eq!(blocked.remedy(NOW), Some(Remedy::ByHand));
    // A wait no rung passes is the owner's setup, not the upgrade's damage: a
    // record (a digest line for the missing shell integration), no tab mark.
    assert!(!blocked.asks_on_its_own(NOW) && blocked.badge(NOW).is_none());
    // NEGATIVE CONTROLS: one look at it is no stall (a tab mid-handoff), and
    // the wait word alone — with no blocker's clock — is none either.
    let fresh = Row {
        blocked_since: NOW - 60,
        ..blocked.clone()
    };
    assert_eq!(fresh.stall(NOW), None);
    let word_only = Row {
        wait: "no-shell-integration".into(),
        wait_since: NOW - BLOCKED_AFTER_S,
        blocked: String::new(),
        blocked_since: 0,
        ..blocked
    };
    assert_eq!(word_only.stall(NOW), None);
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

/// The Codex lane in the status view: a `codex-<tab>` state is a row like
/// Claude's (`--status`, `upgrade=`, `--now|--defer|--skip`), standing only
/// while the tab's Codex TUI runs a build older than the target, and said as
/// Codex's.
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
    assert_eq!(row.move_words(), "Codex 0.157.0 → 0.157.1");
    assert!(
        row.column(now_s())
            .starts_with("pending/0.157.1/daemon-first/")
    );
    assert_eq!(
        row.to_json(now_s()).get("agent").and_then(Value::as_str),
        Some("Codex")
    );
    // It stands only while its tab's Codex is behind: the host hands it over
    // under a Codex holder on 0.157.0, and not once the tab runs 0.157.1.
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
    // The owner's word lands on it, by its tab.
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

/// [`holders`] as the window's host reads them, for `home` under `tabs`.
fn host_holders(
    home: PathBuf,
    tabs: Vec<LiveTab>,
) -> impl Fn(&BTreeSet<String>) -> Option<Vec<Holder>> {
    move |sessions| holders(&home, sessions, Some(&tabs))
}

/// A Codex state for [`TAB`] in `phase`, its TUI seen gone at `exited_at`.
fn codex_state(o: &Opts, phase: Phase, exited_at: u64) -> String {
    let session = format!("codex-{TAB}");
    save(
        o,
        &session,
        &St {
            agent: upgrade::Agent::Codex,
            phase,
            from: "0.157.0".to_string(),
            to: "0.157.1".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            mode: "daemon".to_string(),
            pending_since: now_s() - 600,
            exited_at,
            ..St::default()
        },
    );
    session
}

#[test]
fn a_codex_move_that_failed_after_its_exit_stands_with_no_holder() {
    // A Codex move failing after its `/exit` has no old-build TUI to vet
    // against, yet must still show in `--status`, band and `upgrade=`.
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    // The host's own holder read (the process table, this home's Codex TUIs
    // by the roster): the TUI is gone, so it finds none, as in the window.
    let real = |o: &Opts| host_holders(o.home.clone(), tabs.to_vec());
    for why in [
        "no-resume-hint",
        "hint-mismatch",
        "no-resume",
        "stale-exit",
        "shell-gone",
        "relaunch-refused",
    ] {
        let dir = ScratchDir::new("upgrade-status", &format!("cx-failed-{why}"));
        let o = opts(&dir);
        let exited = now_s() - 120;
        codex_state(&o, Phase::Failed(why.to_string()), exited);
        let (shown, vetted) = status_rows(&o, true);
        assert!(vetted, "{why}");
        assert_eq!(shown.len(), 1, "{why}: listed by --status");
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let mut view = View::default();
        view.refresh_with(&o, &tabs, now_s(), &real(&o), &mut |r| {
            sent.push(r.to_vec());
        });
        let rows = sent.last().cloned().unwrap_or_default();
        assert_eq!(rows.len(), 1, "{why}: handed to the window");
        assert_eq!(rows[0].stall(now_s()), Some(format!("failed:{why}")));
        assert_eq!(rows[0].remedy(now_s()), Some(Remedy::ResumeInTab));
        let words = rows[0].stall_words(now_s()).expect("stalled");
        assert!(!words.starts_with("the move stopped"), "{why}: {words}");
        // A day on, the window lets it go; the ledger keeps it.
        let mut view = View::default();
        view.refresh_with(
            &o,
            &tabs,
            exited + EXITED_FAILURE_SHOWN_S + 1,
            &real(&o),
            &mut |r| sent.push(r.to_vec()),
        );
        assert_eq!(sent.last().map(Vec::len), Some(0), "{why}");
    }
    // Negative control: a failure before any exit (its TUI never went) is
    // still vetted: with no old-build Codex in the tab, nothing is shown.
    let dir = ScratchDir::new("upgrade-status", "cx-failed-before");
    let o = opts(&dir);
    codex_state(&o, Phase::Failed("argv:unknown-flag".to_string()), 0);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &real(&o), &mut |r| {
        sent.push(r.to_vec());
    });
    assert_eq!(sent.last().map(Vec::len), Some(0));
}

#[test]
fn a_codex_move_in_flight_refuses_the_owners_word_in_its_own_terms() {
    // No Codex is ever signalled: its move begins with a typed `/exit`.
    let dir = ScratchDir::new("upgrade-status", "cx-in-flight");
    let o = opts(&dir);
    codex_state(&o, Phase::Exiting { at_s: now_s() }, 0);
    let err = ask(&o, TAB, Ask::Now, false).expect_err("a move in flight");
    assert!(err.contains("once its `/exit` is typed"), "{err}");
    assert!(!err.contains("signalled"), "{err}");
    // One that stopped after its `/exit` names what takes it back — nothing
    // is left in the tab to quit — and `--skip` still quiets it.
    codex_state(
        &o,
        Phase::Failed("no-resume-hint".to_string()),
        now_s() - 60,
    );
    let err = ask(&o, TAB, Ask::Now, false).expect_err("stopped");
    assert!(err.contains("`codex resume` in the tab"), "{err}");
    assert!(!err.contains("quit it"), "{err}");
    let quiet = ask(&o, TAB, Ask::Skip, false).expect("a skip of the record");
    assert_eq!(quiet.stall(now_s()), None);
}

/// A Claude Code state for [`TAB`] in `phase`, its agent seen gone at
/// `exited_at` (`0`: never).
fn claude_state(o: &Opts, phase: Phase, exited_at: u64) -> String {
    let session = "0badf00d-1111-2222-3333-444455556666".to_string();
    save(
        o,
        &session,
        &St {
            phase,
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 600,
            exited_at,
            ..St::default()
        },
    );
    session
}

/// A Claude Code move that failed after its SIGTERM ended the agent stands with
/// no holder, as a Codex one does
/// (`a_codex_move_that_failed_after_its_exit_stands_with_no_holder`), naming
/// `claude --resume` as what takes the conversation back. Negative controls: a
/// failure before any exit is vetted against a holder; once a live process
/// holds the conversation again, it is an ordinary stopped upgrade.
#[test]
fn a_claude_move_that_failed_after_its_exit_stands_with_no_holder() {
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    for why in ["stale-exit", "shell-gone", "no-resume", "relaunch-refused"] {
        let dir = ScratchDir::new("upgrade-status", &format!("cl-failed-{why}"));
        let o = opts(&dir);
        let exited = now_s() - 120;
        claude_state(&o, Phase::Failed(why.to_string()), exited);
        let (shown, vetted) = status_rows(&o, true);
        assert!(vetted, "{why}");
        assert_eq!(shown.len(), 1, "{why}: listed by --status");
        let mut sent: Vec<Vec<Row>> = Vec::new();
        let mut view = View::default();
        view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |r| {
            sent.push(r.to_vec());
        });
        let rows = sent.last().cloned().unwrap_or_default();
        assert_eq!(rows.len(), 1, "{why}: handed to the window");
        assert_eq!(rows[0].stall(now_s()), Some(format!("failed:{why}")));
        assert_eq!(rows[0].remedy(now_s()), Some(Remedy::ResumeInTab), "{why}");
        let words = rows[0].stall_words(now_s()).expect("stalled");
        assert!(!words.starts_with("the move stopped"), "{why}: {words}");
        assert!(!words.contains("Codex"), "{why}: {words}");
        // A person's words (ruling 284): no raw stop word, no parenthesis.
        assert!(
            !words.contains(why) && !words.contains('('),
            "{why}: {words}"
        );
        // Nothing runs in the tab to ask: no round asks it on its own, so it
        // is the tab's standing row, never a record (ruling 283).
        assert!(!rows[0].asks_on_its_own(now_s()), "{why}");
        // The owner's `--now` names what takes it back.
        let err = ask(&o, TAB, Ask::Now, false).expect_err("stopped");
        assert!(err.contains("claude --resume"), "{why}: {err}");
        assert!(!err.contains("quit it"), "{why}: {err}");
        // A day on, the window lets it go; the ledger keeps it.
        let mut view = View::default();
        view.refresh_with(
            &o,
            &tabs,
            exited + EXITED_FAILURE_SHOWN_S + 1,
            &nobody,
            &mut |r| sent.push(r.to_vec()),
        );
        assert_eq!(sent.last().map(Vec::len), Some(0), "{why}");
    }
    // Negative control: before any exit (signal refused, agent alive) it is
    // vetted against a holder.
    let dir = ScratchDir::new("upgrade-status", "cl-failed-before");
    let o = opts(&dir);
    claude_state(&o, Phase::Failed("signal-refused".to_string()), 0);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |r| sent.push(r.to_vec()));
    assert_eq!(sent.last().map(Vec::len), Some(0));
    // Resumed by hand: an ordinary stopped upgrade, vetted by its holder.
    let dir = ScratchDir::new("upgrade-status", "cl-failed-resumed");
    let o = opts(&dir);
    claude_state(&o, Phase::Failed("stale-exit".to_string()), now_s() - 60);
    let held_on = |version: &'static str| {
        move |s: &BTreeSet<String>| -> Option<Vec<Holder>> {
            Some(s.iter().map(|s| holder(s, version, Some(TAB))).collect())
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

/// A new round inherits nothing of the old one's restart ([`St::rearm`]): a
/// re-armed record's new stop before any signal is one whose agent lives,
/// never read as a failure after an exit. Negative control: before the re-arm
/// the old stop is the old restart's.
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

/// A restart under way that does not move reads stalled: past
/// [`super::super::STALE_S`] from its phase's start it reads `stuck:<what>`
/// (checked before the column shows `restarting`), says what it waits on
/// (nothing is forced), and its remedy waits ([`Remedy::Waits`]); its mark is
/// stable and the view wakes at the bound. Negative control: within the bound
/// it reads `restarting`.
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
    for (phase, exited_at, kind, needle) in [
        (
            Phase::Exiting {
                at_s: NOW - stale - 1,
            },
            0,
            "stuck:exiting",
            "has not exited",
        ),
        (
            Phase::Exiting {
                at_s: NOW - stale - 1,
            },
            NOW - stale,
            "stuck:exited",
            "prompt",
        ),
        (
            Phase::Relaunched {
                at_s: NOW - stale - 1,
            },
            NOW - stale - 30,
            "stuck:relaunched",
            "idle point",
        ),
    ] {
        let r = row(phase.clone(), exited_at);
        assert_eq!(r.stall(NOW).as_deref(), Some(kind), "{phase:?}");
        assert_eq!(r.column(NOW), format!("stalled/2.1.282/{kind}/2m"));
        assert_eq!(r.remedy(NOW), Some(Remedy::Waits), "{kind}");
        assert!(!r.moving(NOW), "{kind}");
        let words = r.stall_words(NOW).expect("stalled");
        assert!(words.contains(needle), "{kind}: {words}");
        assert!(words.contains("nothing is forced"), "{kind}: {words}");
        // The band's words ask for no restart (the reporters' guard).
        for asks in ["restart", "relaunch", "kill"] {
            assert!(!words.to_lowercase().contains(asks), "{kind}: {words}");
        }
        // Within the bound of a shutdown nothing is marked; long past it, the
        // agent that was ended and is not back marks its tab (an interrupt),
        // once, and the text does not age. A relaunched agent that has not
        // reached an idle point is the supervisor's box to answer: no mark.
        assert_eq!(r.badge(NOW), None, "{kind}: too soon to mark a tab");
        let late = NOW + 3_600;
        match kind {
            // A relaunched agent that has reached no idle point is the
            // supervisor's box to answer for RELAUNCH_INTERRUPT_S (a fault
            // line at RELAUNCH_STUCK_S); past it only the owner can look at
            // the tab: an interrupt with the one action.
            "stuck:relaunched" => {
                assert_eq!(
                    r.badge(NOW + RELAUNCH_STUCK_S),
                    None,
                    "{kind}: a log line first"
                );
                let mark = r.badge(late).expect("the owner is needed");
                assert!(
                    mark.contains("has not reached a prompt")
                        && mark.ends_with("Look at that tab and answer what it shows"),
                    "{mark}"
                );
                assert_eq!(r.badge(late), r.badge(late + 3_600), "{kind}: stable");
            }
            _ => {
                assert!(r.badge(late).is_some_and(|b| b.len() <= 200), "{kind}");
                assert_eq!(r.badge(late), r.badge(late + 3_600), "{kind}");
            }
        }
    }
    // The owner's view wakes at the bound, by time alone.
    let dir = ScratchDir::new("upgrade-status", "stuck-wake");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let at = now_s() - 10;
    claude_state(&o, Phase::Exiting { at_s: at }, 0);
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let mut view = View::default();
    let next = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    assert_eq!(next, Some(at + stale + 1));
    // A Codex row's words are its own.
    let codex = Row {
        agent: upgrade::Agent::Codex,
        ..row(
            Phase::Exiting {
                at_s: NOW - stale - 1,
            },
            0,
        )
    };
    let words = codex.stall_words(NOW).expect("stalled");
    assert!(words.contains("/exit"), "{words}");
}

/// A Claude Code waiting on its own work is worded for the user within the
/// band's 64 characters at any age below 100 days, and `--now` is not offered.
#[test]
fn a_claude_wait_on_its_own_work_is_worded_for_the_owner() {
    for wait in ["background", "not-idle:shell"] {
        let row = Row {
            wait: wait.to_string(),
            ..pending(10 * 86_400 + 23 * 3_600)
        };
        let words = row.stall_words(NOW).expect("overdue");
        assert!(words.ends_with("its own work runs"), "{wait}: {words}");
        assert!(words.chars().count() <= 64, "{wait}: {words}");
        assert_eq!(row.remedy(NOW), Some(Remedy::Waits), "{wait}");
    }
    // A turn in progress has no such words: its wait word says it.
    let busy = Row {
        wait: "not-idle:busy".to_string(),
        ..pending(7 * 3_600)
    };
    assert!(!busy.stall_words(NOW).expect("overdue").contains("own work"));
    // A status disagreeing with an idle screen is named, and `--now` is not
    // offered for it.
    for status in ["busy", "shell"] {
        let stale = Row {
            wait: format!("status-stale:{status}"),
            ..pending(10 * 86_400 + 23 * 3_600)
        };
        let words = stale.stall_words(NOW).expect("overdue");
        assert!(words.ends_with("its status lags"), "{words}");
        assert!(words.chars().count() <= 64, "{words}");
        assert_eq!(stale.remedy(NOW), Some(Remedy::Waits), "{status}");
    }
}

/// A person at the tab is named only while a look has seen them: a wait no
/// look has confirmed for [`ATTENDED_SEEN_S`] waits for the next turn end
/// instead. Negative control: a look 30 s ago names the person ("using"). An
/// overdue upgrade already stands at the Land rung, which `--now` is, so
/// neither is moved by `Upgrade now`.
#[test]
fn a_person_at_the_tab_is_named_only_while_a_look_has_seen_them() {
    let at = |seen_ago: u64| Row {
        wait: "attended".to_string(),
        wait_since: NOW - 6 * 3_600,
        wait_seen: NOW - seen_ago,
        ..pending(13 * 3_600)
    };
    let stale = at(6 * 3_600).stall_words(NOW).expect("overdue");
    assert_eq!(stale, "behind for 13 h: it waits for its next turn end");
    assert!(
        !stale.contains("typing") && !stale.contains("using"),
        "{stale}"
    );
    let fresh = at(30).stall_words(NOW).expect("overdue");
    assert_eq!(fresh, "behind for 13 h: someone is using its tab");
    assert!(
        at(ATTENDED_SEEN_S)
            .stall_words(NOW)
            .expect("overdue")
            .ends_with("using its tab")
    );
    assert!(
        at(ATTENDED_SEEN_S + 1)
            .stall_words(NOW)
            .expect("overdue")
            .ends_with("its next turn end")
    );
    // A state an older build wrote (no look recorded): its start decides.
    let unrecorded = Row {
        wait_seen: 0,
        ..at(0)
    };
    assert!(
        unrecorded
            .stall_words(NOW)
            .expect("overdue")
            .ends_with("its next turn end")
    );
    for ago in [30, 6 * 3_600] {
        assert_eq!(at(ago).remedy(NOW), Some(Remedy::Waits), "{ago}");
    }
    // THE TAB'S MARK SAYS WHAT THE ROW SAYS: a person no look has seen lately
    // is no person on the mark either. It changes once, at the instant the
    // row does, and not again.
    let mark = |seen_ago: u64| {
        assert_eq!(at(seen_ago).badge(NOW), None, "a wait marks no tab");
        at(seen_ago).overdue_cause(NOW)
    };
    assert!(mark(30).ends_with("waiting on a person (a question, a draft, a login or a hold)"));
    for ago in [ATTENDED_SEEN_S + 1, 6 * 3_600] {
        assert!(mark(ago).ends_with("held by its own work"), "{}", mark(ago));
        assert!(!mark(ago).contains("person"), "{ago}");
    }
    assert!(
        mark(ATTENDED_SEEN_S).contains("waiting on a person"),
        "seen at the bound"
    );
    assert_eq!(
        mark(ATTENDED_SEEN_S + 1),
        mark(6 * 3_600),
        "no churn past it"
    );
    assert!(
        unrecorded
            .overdue_cause(NOW)
            .ends_with("held by its own work")
    );
    // A Codex tab's person is named by the word alone, row and mark alike.
    let codex = Row {
        agent: upgrade::Agent::Codex,
        ..at(6 * 3_600)
    };
    assert!(
        codex
            .overdue_cause(NOW)
            .ends_with("waiting on a person (a question, a draft, a login or a hold)")
    );
}

/// The user is told what holds the move: `--status` carries `held_by=` (pid,
/// name and age; never the command, which is quoted to the agent alone), its
/// JSON a list, and the row the band's words. Each process's start is kept so
/// the same shells read the same row every look. At most five are named, the
/// rest counted. Negative control: nothing held reads `held_by=-` and no words.
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
    assert_eq!(
        row.held_words(NOW).as_deref(),
        Some("pid 63492 (zsh, 5d4h); pid 63493 (zsh, 5d4h)")
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
    // Its age moves; the row does not.
    assert_eq!(Row { ..row.clone() }, row);
    let many = Row {
        held_by: (1..=7).map(shell).collect(),
        ..row.clone()
    };
    assert!(
        many.line(NOW).contains(",5(zsh:5d4h),+2 release=- "),
        "{}",
        many.line(NOW)
    );
    assert!(
        many.held_words(NOW)
            .is_some_and(|w| w.ends_with("; and 2 more")),
        "{:?}",
        many.held_words(NOW)
    );
    // Negative control: nothing held.
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

/// A release still owed is shown: an owed release (`St::release`) means an
/// agent asked to wind down is stopped now, waiting to be told to carry on.
/// `--status` carries `release=<why>` (before the final `rung=`), `--json`
/// carries `release`, the wait reads as words, and the gave-up words say the
/// line is owed. Negative controls: a released round reads `release=-`; a
/// Codex record (no release lane) reads `release=-`.
#[test]
fn a_release_still_owed_is_shown_and_worded() {
    let owed = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 8 * 3_600,
        failed_at: NOW - 600,
        // The word a give-up owes it under (`St::owe_release`).
        release: "gave-up".to_string(),
        wait: "release:draft".to_string(),
        wait_since: NOW - 300,
        ..St::default()
    };
    let row = Row::of("aaa", &owed, NOW);
    let line = row.line(NOW);
    assert!(line.contains(" wait=release:draft wait_for=5m "), "{line}");
    assert!(line.contains(" held_by=- release=gave-up "), "{line}");
    assert_eq!(
        row.to_json(NOW).get("release").and_then(Value::as_str),
        Some("gave-up")
    );
    let words = row.stall_words(NOW).expect("overdue");
    assert!(words.starts_with("behind for 8 h: "), "{words}");
    assert!(
        words.contains("the line that tells it to carry on"),
        "{words}"
    );
    assert!(!words.contains("release"), "no raw word: {words}");
    // The release's own gate holding the line on a still-asking round: its
    // words fit the band's first line.
    let held = Row {
        wait: "release:draft".to_string(),
        release: "retargeted".to_string(),
        ..pending(10 * 86_400 + 23 * 3_600)
    };
    let words = held.stall_words(NOW).expect("overdue");
    assert!(words.contains("carry on"), "{words}");
    assert!(!words.contains("release"), "{words}");
    assert!(words.chars().count() <= 64, "{words}");
    assert_eq!(held.remedy(NOW), Some(Remedy::Waits), "--now types no line");
    // A gate's word left over a release nothing owes says no line is owed.
    let stale = Row {
        release: String::new(),
        ..held.clone()
    };
    let words = stale.stall_words(NOW).expect("overdue");
    assert!(!words.contains("carry on"), "nothing is owed: {words}");
    assert!(!words.contains("owed"), "nothing is owed: {words}");
    // Negative controls.
    let released = Row::of(
        "aaa",
        &St {
            release: String::new(),
            wait: "failed".to_string(),
            ..owed.clone()
        },
        NOW,
    );
    assert!(
        released.line(NOW).contains(" held_by=- release=- "),
        "{}",
        released.line(NOW)
    );
    assert_eq!(
        released.to_json(NOW).get("release").and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        released.stall_words(NOW).as_deref(),
        Some(
            "behind for 8 h: it has not agreed to the move yet, so the upgrade rests, then asks again"
        )
    );
    let codex = Row::of(
        "codex-s-b5cf2faabac5ce5127bd",
        &St {
            agent: upgrade::Agent::Codex,
            ..owed.clone()
        },
        NOW,
    );
    assert!(
        codex.line(NOW).contains(" release=- "),
        "{}",
        codex.line(NOW)
    );
}

/// NO RAW WAIT WORD IN A PERSON'S WORDS (never `waiting (not-idle:busy)`):
/// every wait a Claude step records reads as words, a wait with none is left
/// out, and the words fit the band's first line. An ANNOUNCED move on its own
/// work is still asking by itself: it marks no tab — and neither does the same
/// wait before any notice, nor a turn still running (behind for hours is no
/// reason to mark a tab).
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
        "something-new",
    ] {
        let row = Row {
            wait: wait.to_string(),
            ..pending(10 * 86_400 + 23 * 3_600)
        };
        let words = row.stall_words(NOW).expect("overdue");
        assert!(!words.contains('('), "{wait}: {words}");
        // A machine word (`not-idle:busy`, `awaiting-ready`) never reads.
        assert!(
            !wait.contains([':', '-']) || !words.contains(wait),
            "{wait}: {words}"
        );
        assert!(words.chars().count() <= 64, "{wait}: {words}");
    }
    // Nothing holds it: its words say when it goes, since a grouped band
    // prints no remedy line beside them.
    let next = Row {
        wait: upgrade::NEXT_POINT.to_string(),
        ..pending(10 * 86_400 + 23 * 3_600)
    };
    assert_eq!(
        next.stall_words(NOW).as_deref(),
        Some("behind for 10 days: its next step goes at its next pause")
    );
    assert!(NEXT_POINT_CAUSE.ends_with(NEXT_POINT_WORDS));
    let asked = |wait: &str| Row {
        phase: Phase::Announced { at_s: NOW, asks: 1 },
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    };
    assert!(asked("background").asks_on_its_own(NOW));
    assert_eq!(asked("background").badge(NOW), None);
    assert!(!pending_with("background").asks_on_its_own(NOW));
    assert!(pending_with("background").badge(NOW).is_none());
    // An announced move whose turn is still running is the upgrade asking
    // on its own too (ruling 380): an
    // overdue upgrade stands on the ladder's last rung, which `--now` is, so
    // no word of the owner's moves it sooner. A record, no tab mark — as is
    // every wait: only an interrupt marks a tab.
    assert!(asked("not-idle:busy").asks_on_its_own(NOW));
    assert_eq!(asked("not-idle:busy").badge(NOW), None);
    assert!(!pending_with("not-idle:busy").asks_on_its_own(NOW));
    assert!(pending_with("not-idle:busy").badge(NOW).is_none());
}

/// A limit whose reset no look read is bounded: with `limit_until` 0, a
/// `limited` wait is told `LIMIT_GRACE_S` past a week from when it began (no
/// limit lasts longer), in words naming no reset; the view names that instant.
/// Negative controls: a young wait is still a record, an unknown start is
/// never told, and a named future reset wins.
#[test]
fn a_limit_whose_reset_no_look_read_is_told_once_any_limit_has_ended() {
    let row = |since: u64| Row {
        wait_since: since,
        limit_until: 0,
        behind_since: NOW - 9 * 24 * 3_600,
        ..pending_with("limited")
    };
    let long = row(NOW - LONGEST_LIMIT_S - LIMIT_GRACE_S - 60);
    assert!(long.limit_passed(NOW));
    assert_eq!(long.stall(NOW).as_deref(), Some("overdue"));
    assert!(!long.asks_on_its_own(NOW), "no longer a record");
    assert_eq!(long.badge(NOW), None, "a fault line in the log, no mark");
    assert!(
        matches!(long.attention(NOW), Attention::Fault(ref n) if n.kind == "limit-passed"),
        "{:?}",
        long.attention(NOW)
    );
    let words = long.stall_words(NOW).expect("its words");
    assert!(words.contains("outlasted any usage limit"), "{words}");
    assert!(
        !words.contains("reset passed"),
        "no reset time invented: {words}"
    );

    // Negative control: a young wait is a record; the view wakes at the bound.
    let began = NOW - 3_600;
    let young = row(began);
    assert!(!young.limit_passed(NOW));
    assert!(
        young.asks_on_its_own(NOW),
        "a record until any limit can have ended"
    );
    assert_eq!(young.attention(NOW), Attention::Quiet);
    let bound = began + LONGEST_LIMIT_S + LIMIT_GRACE_S;
    let mut t = NOW;
    let mut named = false;
    while let Some(next) = next_change(std::slice::from_ref(&young), t) {
        if next >= bound {
            named = next == bound;
            break;
        }
        t = next;
    }
    assert!(named, "the view names the bound's instant");
    // Negative controls: an unknown start, and a named reset still ahead.
    assert!(!row(0).limit_passed(NOW), "an unknown start is never told");
    let ahead = Row {
        limit_until: NOW + 3_600,
        ..row(NOW - LONGEST_LIMIT_S - LIMIT_GRACE_S - 60)
    };
    assert!(
        !ahead.limit_passed(NOW),
        "the named reset wins over the bound"
    );
}

fn pending_with(wait: &str) -> Row {
    Row {
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    }
}

/// AN OVERDUE MOVE NAMES ITS CAUSE, BY CLASS (design record 2026-09-28, §1.4),
/// not just "behind for more than 6h". The class
/// ([`Row::cause`], in words [`Row::overdue_cause`]) is the agent's own
/// work, a person, a usage limit, aterm's own notice, a wait no look has
/// recorded, and — loud — a word this build does not classify; one text per
/// class whatever the word under it. An overdue tab is NEVER MARKED — the
/// class is what the digest line, `--status` and the log say
/// (`an_overdue_tab_is_never_marked`).
#[test]
fn an_overdue_move_names_its_cause_by_class() {
    let mark = |wait: &str| {
        assert_eq!(pending_with(wait).badge(NOW), None, "{wait}: never marked");
        pending_with(wait).overdue_cause(NOW)
    };
    let cause = |wait: &str| mark(wait);
    let classes: [(&[&str], &str); 5] = [
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
            ],
            "held by its own work",
        ),
        (
            &[
                "not-idle:waiting",
                "box",
                "draft",
                "login",
                "held",
                "release:draft",
            ],
            "waiting on a person (a question, a draft, a login or a hold)",
        ),
        (
            &["limited", "release:limited"],
            "paused by a usage limit until its reset",
        ),
        (
            &["announce-refused:changed", "announce-refused:yield"],
            "aterm could not type its notice",
        ),
        (&[""], "no look has recorded what it waits on"),
    ];
    for (waits, words) in classes {
        for wait in waits {
            // A limit's pending round is a record, never marked (ruling
            // 307): the class is read on an announced one.
            let got = if *wait == "limited" {
                Row {
                    phase: Phase::Announced { at_s: NOW, asks: 1 },
                    ..pending_with(wait)
                }
                .overdue_cause(NOW)
            } else {
                cause(wait)
            };
            assert_eq!(got, words, "{wait}");
        }
    }
    // (A person at the tab is a person only while a look has seen them:
    // `a_person_at_the_tab_is_named_only_while_a_look_has_seen_them`.)
    // Processes recorded under it, no word: its own work.
    let held = Row {
        held_by: vec![HeldBy {
            pid: 51_435,
            name: "zsh".to_string(),
            since: NOW - 4 * 86_400,
        }],
        ..pending_with("")
    };
    assert_eq!(held.overdue_cause(NOW), "held by its own work");
    // An unclassified word fails loud, named.
    assert_eq!(
        cause("something-new"),
        "a wait aterm does not classify (something-new)"
    );
    // Two classes, two texts.
    assert_ne!(mark("not-idle:busy"), mark("box"));
    // Words that must not fall to "does not classify": the Codex thread's
    // own turn, a resting round, and the waits
    // on a tab or conversation aterm cannot reach.
    for (wait, words) in [
        ("busy-thread", "held by its own work"),
        ("failed", "held by its own work"),
        ("tab-not-live", "aterm cannot reach its tab to ask"),
        ("no-socket", "aterm cannot reach its tab to ask"),
        (
            "conversation-in-other-tab",
            "aterm cannot reach its tab to ask",
        ),
        (
            "notice-owned-by-other-process",
            "aterm cannot reach its tab to ask",
        ),
        ("thread-locked", "aterm cannot reach its tab to ask"),
    ] {
        assert_eq!(pending_with(wait).overdue_cause(NOW), words, "{wait}");
    }
}

/// THE CODEX LADDER'S AND THE GOAL PAUSE'S WORDS ARE CLASSED (rulings 380 and
/// 381): each wait the Codex lane's ladder and goal pause record reads, on an
/// overdue mark, as the class of what holds the move — never "a wait aterm
/// does not classify". A goal, the pause's own steps and the tab's own
/// conversation's turn are its own work; a save-then-wait switch is the
/// usage limit it waits out; a goal aterm paused and left waits on the hand
/// step its own row names; the daemon's waits are the daemon's; a screen or
/// shell integration aterm cannot read, and a move already under way, are
/// aterm's. NEGATIVE CONTROL: a word still untaught stays loud.
#[test]
fn an_overdue_codex_mark_classes_the_ladders_and_goal_pauses_words() {
    let codex = |wait: &str| Row {
        agent: upgrade::Agent::Codex,
        ..pending_with(wait)
    };
    for (wait, class) in [
        ("goal", "held by its own work"),
        ("goal:record", "held by its own work"),
        ("goal:no-shell-integration", "held by its own work"),
        ("goal-held", "held by its own work"),
        ("goal-pausing", "held by its own work"),
        ("goal-pausing:no-fence", "held by its own work"),
        ("goal-pause-refused:held", "held by its own work"),
        ("goal-resume", "held by its own work"),
        ("goal-resuming", "held by its own work"),
        ("daemon-turn", "held by its own work"),
        ("changed", "held by its own work"),
        ("switch", "paused by a usage limit until its reset"),
        ("goal:switch", "paused by a usage limit until its reset"),
        (
            "goal-left-paused",
            "its paused goal waits for a person to resume it",
        ),
        (
            "goal-sandboxed",
            "its paused goal waits for a person to resume it",
        ),
        (
            "daemon-busy",
            "waiting on its Codex daemon, shared with other tabs",
        ),
        ("no-daemon", "waiting on its Codex daemon"),
        ("daemon-version", "waiting on its Codex daemon"),
        ("pin:busy-thread", "waiting on its Codex daemon"),
        (
            "no-shell-integration",
            "aterm cannot read its tab well enough to move it",
        ),
        (
            "screen-unreadable",
            "aterm cannot read its tab well enough to move it",
        ),
        ("in-flight", "its move is already under way"),
    ] {
        let row = codex(wait);
        assert_eq!(row.overdue_cause(NOW), class, "{wait}");
        if let Some(mark) = row.badge(NOW) {
            assert!(!mark.contains("does not classify"), "{wait}: {mark}");
            assert!(mark.len() <= 200, "{wait}");
        }
    }
    // Negative control: an untaught word is still named, loud.
    assert_eq!(
        codex("goal-brand-new").overdue_cause(NOW),
        "a wait aterm does not classify (goal-brand-new)"
    );
}

/// ONE STALL ON ITS OWN WORK IS ONE MARK, WHATEVER ITS GATES SAID: a stall
/// passes through `background` at a break, `announce-refused:changed` where
/// the notice's fence refused at an idle point, and `attended`/`draft` when a
/// person touched the tab; read from the word alone, each would move the class
/// and so the digest line's words. While processes run under the agent those passing words read
/// as its own work; with none recorded, the same words still name their own
/// class — the branch the precedence adds, shown working.
#[test]
fn an_overdue_move_holds_its_class_through_the_notice_paths_passing_words() {
    let zsh = HeldBy {
        pid: 51_435,
        name: "zsh".to_string(),
        since: NOW - 3 * 86_400,
    };
    let under = |wait: &str| Row {
        held_by: vec![zsh.clone()],
        ..pending_with(wait)
    };
    let words = [
        "background",
        "announce-refused:changed",
        "attended",
        "announce-refused:yield",
        "failed",
        "",
    ];
    let first = under(words[0]).overdue_cause(NOW);
    assert_eq!(first, "held by its own work");
    for wait in words {
        assert_eq!(under(wait).overdue_cause(NOW), first, "{wait}");
        assert_eq!(under(wait).badge(NOW), None, "{wait}: never marked");
    }
    // A PERSON'S DRAFT AND A HOLD STAND:
    // text a person typed and did not send, or a halt or another driver's
    // lease, is theirs until they end it, whatever runs under the agent — the
    // draft that was a misread (`a_displaced_grid_never_makes_a_placeholder_a_
    // draft`) is fixed at its source, so a `draft` that stands is real. The
    // class of the ROUND still holds through a look that finds one
    // (`the_agents_own_work_is_the_rounds_not_the_last_looks`).
    for wait in [
        "draft",
        "held",
        "announce-refused:draft",
        "announce-refused:held",
        "announce-refused:box",
    ] {
        assert_eq!(
            under(wait).overdue_cause(NOW),
            "waiting on a person (a question, a draft, a login or a hold)",
            "{wait}"
        );
    }
    // Nothing under it: the word's own class; a person counts only if a look
    // saw them lately
    // (`a_person_at_the_tab_is_named_only_while_a_look_has_seen_them`).
    let seen = Row {
        wait_seen: NOW,
        ..pending_with("attended")
    };
    assert_ne!(seen.overdue_cause(NOW), first);
    assert_ne!(
        pending_with("announce-refused:changed").overdue_cause(NOW),
        first
    );
    // A standing box or limit is a hand that must act, named over the
    // processes: a real change of hands, and a new text.
    assert_eq!(
        under("box").overdue_cause(NOW),
        "waiting on a person (a question, a draft, a login or a hold)"
    );
}

#[test]
fn a_codex_wait_on_its_daemon_is_worded_for_the_owner() {
    let row = |wait: &str| Row {
        agent: upgrade::Agent::Codex,
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    };
    for (wait, needle) in [
        ("daemon-first:busy-thread", "codex agents"),
        ("daemon-first:background-terminal", "/stop"),
        ("daemon-first:owner-held", "owner's word"),
        ("daemon-first:unseen-client", "does not see"),
        ("background-terminal", "/ps"),
        ("background", "the move waits for it"),
        ("left-typed-backoff", "--now"),
    ] {
        let words = row(wait).stall_words(NOW).expect("overdue");
        assert!(words.contains(needle), "{wait}: {words}");
    }
    // `--now` lifts only the back-off of the lot.
    assert_eq!(row("left-typed-backoff").remedy(NOW), Some(Remedy::Now));
    assert_eq!(
        row("daemon-first:busy-thread").remedy(NOW),
        Some(Remedy::Waits)
    );
    // A Claude row's words are its own.
    let claude = Row {
        wait: "background-terminal".to_string(),
        ..pending(7 * 3_600)
    };
    assert!(!claude.stall_words(NOW).expect("overdue").contains("/ps"));
}

/// A Codex goal the move paused, as the user reads it (`goal_hold` read into the
/// row): while held, the record says aterm paused it briefly and resumes it;
/// after a hold ending without the move, the goal runs again and the move is
/// retried; a goal left paused past `GOAL_LEFT_AFTER_S` is a row of its own
/// (`goal-paused`) with the hand step's remedy. Negative controls: a hold
/// within its time is no stall; the switch's hold is not the row's; a Claude
/// row reads no goal.
#[test]
fn a_goal_the_move_paused_is_said_and_one_left_paused_is_a_row() {
    use crate::harness::goal_hold::{Hold, How, Owner, Stage};
    use crate::harness::upgrade_codex as cx;
    let codex = |wait: &str| Row {
        agent: upgrade::Agent::Codex,
        wait: wait.to_string(),
        ..pending(3 * 3_600)
    };
    let paused = Hold {
        stage: Stage::Paused,
        took_at: NOW - 60,
        ..Hold::pausing(Owner::Upgrade, How::Typed, 42, "0.158.0", NOW - 90)
    };
    let held = codex("goal-held").with_goal(Some(&paused), false, NOW);
    assert_eq!(held.goal_held_since, NOW - 90);
    assert_eq!(held.goal_left_since, 0);
    assert_eq!(held.stall(NOW), None, "within its time");
    let line = held.waiting_line("2:15 PM");
    assert!(
        line.starts_with("aterm paused its goal for a moment to install the new Codex")
            && line.ends_with("nothing to do"),
        "{line}"
    );
    // Whatever the step's own word while it stands.
    let line = codex("daemon-turn")
        .with_goal(Some(&paused), false, NOW)
        .waiting_line("2:15 PM");
    assert!(line.starts_with("aterm paused its goal"), "{line}");
    // Left paused: a row, whatever the owner's word.
    let late = NOW + cx::GOAL_LEFT_AFTER_S;
    let left = Row {
        request: Request::Skip("0.158.0".into()),
        ..codex("goal-resume")
    }
    .with_goal(Some(&paused), false, late);
    assert_eq!(left.goal_left_since, NOW - 90 + cx::GOAL_LEFT_AFTER_S);
    assert_eq!(left.stall(late).as_deref(), Some("goal-paused"));
    assert_eq!(left.remedy(late), Some(Remedy::ByHand));
    let why = left.stall_words(late).expect("words");
    assert!(why.contains("has not been able to resume it yet"), "{why}");
    // Never while aterm still holds it as it should: a switch open on the tab,
    // the paused goal's last turn running, a resume just made, the switch's
    // own wait. The waiting record instead.
    let under_switch = codex("goal-resume").with_goal(Some(&paused), true, late);
    assert_eq!(
        (under_switch.goal_left_since, under_switch.stall(late)),
        (0, None)
    );
    for holding in ["goal-held", "goal-resuming", "switch", "goal:switch"] {
        let row = codex(holding).with_goal(Some(&paused), false, late);
        assert_eq!(
            (row.goal_left_since, row.stall(late)),
            (0, None),
            "{holding}"
        );
    }
    // Left paused on purpose (its thread fell into a sandbox): a row at once,
    // with the relaunch as its hand step.
    let sandboxed = codex(cx::SANDBOXED).with_goal(Some(&paused), false, NOW);
    assert_eq!(sandboxed.stall(NOW).as_deref(), Some("goal-sandboxed"));
    assert_eq!(sandboxed.remedy(NOW), Some(Remedy::ByHand));
    assert!(
        sandboxed
            .stall_words(NOW)
            .is_some_and(|w| w.contains("fell into a sandbox")),
        "{:?}",
        sandboxed.stall_words(NOW)
    );
    // Its resumes spent: a row too, at once past the last one's take window.
    let spent = Hold {
        stage: Stage::Resuming,
        resumes: cx::MAX_RESUMES,
        resume_at: NOW - cx::RESUME_TAKE_S,
        ..paused.clone()
    };
    assert_eq!(
        codex("goal-left-paused")
            .with_goal(Some(&spent), false, NOW)
            .stall(NOW)
            .as_deref(),
        Some("goal-paused")
    );
    // Resumed without the move, resting: said so, no row.
    let rested = Hold {
        stage: Stage::Resumed,
        resume_at: NOW - 60,
        resumes: 1,
        why: "abandoned:failed".into(),
        ..paused.clone()
    };
    let resting = codex("goal").with_goal(Some(&rested), false, NOW);
    assert_eq!(resting.goal_rest_until, NOW - 60 + cx::GOAL_REST_S);
    // The ask flow (`upgrade_ask = true`, not `quiet`): aterm pauses it
    // again later.
    let asking = Row {
        quiet: false,
        ..resting.clone()
    };
    assert!(
        asking.waiting_line("2:15 PM").contains("tries again later"),
        "{}",
        asking.waiting_line("2:15 PM")
    );
    // Under the default quiet flow no new pause comes: the goal holds it
    // while it runs.
    let quiet = Row {
        quiet: true,
        ..resting.clone()
    }
    .waiting_line("2:15 PM");
    assert!(
        !quiet.contains("tries again")
            && quiet.ends_with("it moves once the goal stops: `/goal pause` in this tab stops it"),
        "{quiet}"
    );
    // The switch's hold is none of the row's; a Claude row reads no goal.
    let switch = Hold {
        owner: Owner::Switch,
        ..paused.clone()
    };
    let row = codex("goal").with_goal(Some(&switch), false, late);
    assert_eq!((row.goal_held_since, row.goal_left_since), (0, 0));
    let claude = pending(3 * 3_600).with_goal(Some(&paused), false, late);
    assert_eq!((claude.goal_held_since, claude.goal_left_since), (0, 0));
}

/// `--status`'s `rung=`: the ladder's rung while the move is owed, `land` under
/// `--now`, `-` once under way, done or stopped; followed by the watch's
/// fields (`looked=` first). The JSON carries it too.
#[test]
fn the_status_line_says_the_rung() {
    for (behind, rung) in [
        (60, "prefer"),
        (upgrade::RUNG_SETTLED_S, "settled"),
        (upgrade::RUNG_KEYS_S, "keys-only"),
        (upgrade::RUNG_LAND_S, "land"),
    ] {
        let line = pending(behind).line(NOW);
        assert!(line.contains(&format!(" rung={rung} looked=")), "{line}");
        assert_eq!(
            pending(behind)
                .to_json(NOW)
                .get("rung")
                .and_then(Value::as_str),
            Some(rung)
        );
    }
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 10,
        ..pending(60)
    };
    assert!(hurried.line(NOW).contains(" rung=land looked="));
    let under_way = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(60)
    };
    assert!(under_way.line(NOW).contains(" rung=- looked="));
}

/// A word also holds a gave-up upgrade's late READY: a gave-up upgrade still
/// honours a READY (`upgrade::next_step`), so `--skip` or `--defer` forgets
/// them. Negative control: left alone, the record keeps them.
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

/// A held-back agent names what it runs under (`tmux`), and one whose terminal
/// owner has no name to give (`terminal:none`, or `terminal:?` read back as
/// `held-back:-`) says it runs under a terminal that is not the tab's, never
/// "runs under none".
#[test]
fn a_held_back_agent_without_an_owner_name_reads_as_a_sentence() {
    assert_eq!(
        stall_reason(upgrade::Agent::Claude, "held-back:tmux"),
        "it runs inside tmux, where the upgrade cannot type to it"
    );
    for kind in ["held-back:none", "held-back:-"] {
        assert_eq!(
            stall_reason(upgrade::Agent::Claude, kind),
            "it runs inside a terminal that is not the tab's, where the upgrade cannot type to \
             it",
            "{kind}"
        );
    }
    assert_eq!(runs_under("?"), "a terminal that is not the tab's");
    assert_eq!(runs_under("screen"), "screen");
}

/// When the next round starts: every stopped round's row says it
/// (`next_round=<span>`, `next_round_s` and `retry_at` in JSON,
/// `next-round:<span>` in the column of a non-stall), and `due` once rested (an
/// unstamped stop is due at once). A skipped round has no next round (`-`), nor
/// a gave-up round acting on a late READY (`ready` in the column). A refusal
/// stays a stall through the re-armed round's first look (`last_stop`) until
/// its first notice, so its band row does not flap at every re-arm.
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
    // 20 hours behind (inside the move clock, `MOVE_BUDGET_S`): the rest is
    // no stall of its own, and its age reads `overdue` as every round's does.
    assert_eq!(row.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(row.next_round_in(NOW), Some(upgrade::RETRY_S - 3_600));
    assert!(
        row.line(NOW)
            .contains(" request=- next_round=1h stalled=overdue held_by=- release=- "),
        "{}",
        row.line(NOW)
    );
    // The column says pending on its next round, however far behind; the line
    // keeps the stall word and `next_round=`.
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
    // Unstamped (an older build's): due at once.
    let old = Row::of(
        "aaa",
        &St {
            failed_at: 0,
            ..gave_up.clone()
        },
        NOW,
    );
    assert_eq!(old.next_round_in(NOW), Some(0));
    assert!(
        old.line(NOW).contains(" next_round=due "),
        "{}",
        old.line(NOW)
    );
    assert_eq!(old.column(NOW), "pending/2.1.282/next-round:due/20h");
    // The owner's skip: no next round, and no stall.
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
    assert!(
        skipped.line(NOW).contains(" next_round=- "),
        "{}",
        skipped.line(NOW)
    );
    // A late READY a gave-up round heard: no new round starts while it stands,
    // so none is `due`; it is still asking on its own (`ready`), and an overdue
    // one's words say so. Negative control: a gave-up Codex round hears no late
    // READY and is due.
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
    assert!(
        late.line(NOW).contains(" next_round=- "),
        "{}",
        late.line(NOW)
    );
    assert_eq!(late.column(NOW), "pending/2.1.282/ready/20h");
    assert!(late.asks_on_its_own(NOW), "the upgrade working, not a row");
    // A person's units in the words (ruling 308); the column keeps its own.
    assert_eq!(
        late.stall_words(NOW).as_deref(),
        Some(
            "behind for 20 h: it agreed to the move after the upgrade stopped asking, and the \
             upgrade acts on that answer (its turn is still running)"
        )
    );
    assert!(
        row.stall_words(NOW)
            .is_some_and(|w| w.ends_with("so the upgrade rests, then asks again")),
        "{:?}",
        row.stall_words(NOW)
    );
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
    // Nothing else stopped has a next round.
    assert_eq!(Row::of("aaa", &base, NOW).next_round_in(NOW), None);

    // A refusal: a stall with its next round; re-armed, the same stall until
    // the new round's first notice (an hour behind, since past six hours a
    // pending round reads `overdue`).
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
    assert!(
        row.line(NOW).contains(" next_round=due "),
        "{}",
        row.line(NOW)
    );
    let mut st = refused.clone();
    assert_eq!(st.rearm(NOW), "not-a-shell-job");
    assert_eq!(
        Row::of("aaa", &st, NOW).stall(NOW).as_deref(),
        Some("refused:not-a-shell-job"),
        "no flap at the re-arm"
    );
    st.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), NOW, 1);
    assert_eq!(Row::of("aaa", &st, NOW).stall(NOW), None, "met no refusal");
    // A give-up re-armed is no stall at any look.
    let mut st = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..young
    };
    let _ = st.rearm(NOW);
    assert_eq!(Row::of("aaa", &st, NOW).stall(NOW), None);
}

/// A round that stopped and will ask again on its own is the upgrade working
/// and marks no tab. A gave-up round keeps `Upgrade now` even when `overdue`; a
/// first stop for another reason is asked again after its rest. A repeating
/// stop is the person's: it carries its stall through the re-armed round and
/// marks the tab. Negative controls: a refusal marks from its first stop; a
/// gave-up round the user skipped is held, not asking.
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
    assert_eq!(
        once.stall_words(NOW).as_deref(),
        Some("the system refused to stop the old Claude")
    );

    let twice = Row {
        stop_streak: 2,
        ..once.clone()
    };
    assert!(twice.repeating_stop() && !twice.asks_on_its_own(NOW));
    // The column agrees with the band: a round asking again on its own is
    // pending on its next round; a repeating stop is the stall.
    assert_eq!(gave_up.column(NOW), "pending/2.1.282/next-round:10m/8h");
    assert_eq!(once.column(NOW), "pending/2.1.282/next-round:10m/8h");
    assert_eq!(
        twice.column(NOW),
        "stalled/2.1.282/failed:signal-refused/8h"
    );
    // A stop that repeats is ONE line in the digest, never a mark on the tab:
    // it can wait, and the owner's action is theirs.
    assert_eq!(twice.badge(NOW), None);
    assert!(
        matches!(twice.attention(NOW), Attention::Digest(n) if n.kind == "repeating-stop"),
        "{:?}",
        twice.attention(NOW)
    );
    // Re-armed: the stall is carried through the new round (one entry).
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
        // Negative control: a first stop's re-armed round carries nothing.
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

/// A PERSON'S HOLD IS NOT THE UPGRADE ASKING ON ITS OWN, AND IT IS NOT AN
/// ALERT EITHER (an alert persists only where the owner can act, and says why
/// and what). The row's STATE is exact: an
/// announced round whose notice waits on a draft, a box, aterm's hold, a
/// login or a question is never asked again (the re-ask waits under the same
/// gate), so it never gives up either, and only the person ends it. Its
/// LOUDNESS is the classifier's: the person who holds it is at the tab, so a
/// hold is a record while the last notice is fresh, ONE log line naming the
/// waits once a day has passed with no progress, and — a draft nobody sent,
/// stood for a day — ONE digest line. Never a tab mark, never a row (design
/// record 2026-09-29, D14).
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
        // The hold has STOOD: the same word a whole REASK_S and the last ask
        // that old; the last notice is an hour old.
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
        // A DAY WITH NO PROGRESS: one fault line in the log, quoting the
        // waits — a record, never a row or a mark.
        let stale = Row {
            phase: Phase::Announced {
                at_s: NOW - BACKSTOP_S,
                asks: 1,
            },
            ..row.clone()
        };
        assert!(!stale.asks_on_its_own(NOW), "{wait}");
        assert_eq!(stale.badge(NOW), None, "{wait}");
        assert!(
            matches!(stale.attention(NOW), Attention::Fault(n) if n.kind == "no-progress"),
            "{wait}: {:?}",
            stale.attention(NOW)
        );
        assert_eq!(stale.interrupt(NOW), None, "{wait}");
    }
    // A DRAFT NOBODY SENT, a day on: ONE line in the digest, still no mark.
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
    // NEGATIVE CONTROL: the agent's own work, and a limit, still ask on
    // their own inside the move clock.
    for wait in ["background", "awaiting-ready", "limited"] {
        let row = Row {
            phase: Phase::Announced {
                at_s: NOW - 600,
                asks: 1,
            },
            wait: wait.into(),
            // (a READY answer that has waited seven hours is aterm's fault,
            // told in the log: this control is a wait that just began)
            wait_since: NOW - 600,
            ..pending(STALLED_AFTER_S + 3_600)
        };
        assert!(row.asks_on_its_own(NOW), "{wait}");
        assert_eq!(row.badge(NOW), None, "{wait}");
        assert_eq!(row.attention(NOW), Attention::Quiet, "{wait}");
    }
    // Negative control: a passing hold (a person typing at one idle look) is
    // the gate working: a fresh `draft` after a ten-minute-old ask, or a
    // minute-old one after a stale ask, still asks on its own with no mark;
    // the view wakes when it has stood.
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

/// THE MOVE CLOCK ([`MOVE_BUDGET_S`]): a session whose own
/// work never ends is asked, given up on, rested and asked again for as long
/// as it runs. A day behind, the row's WORDS say so — nothing is "pending on
/// its next round" for a day, whatever its rounds do — and the classifier
/// makes the day news, once: an agent whose own work has answered a whole
/// round with "not yet" a day behind is ONE digest line (`own-work`), never a
/// row, a mark or an interrupt (design record 2026-09-29, D14). A usage
/// limit stays a record until its reset (ruling 307).
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
    assert!(
        !gave_up.asks_on_its_own(NOW),
        "five rounds given up in a row, 3 days behind, is not the upgrade working"
    );
    assert!(
        gave_up.will_ask_again(NOW),
        "but it does ask again on its own, at any age: the words of its record say so"
    );
    assert_eq!(gave_up.badge(NOW), None, "one digest line, never a mark");
    assert!(
        matches!(gave_up.attention(NOW), Attention::Digest(n) if n.kind == "own-work"),
        "{:?}",
        gave_up.attention(NOW)
    );
    // The round still asking: the digest comes when the round is answered.
    let asking = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 3,
        },
        wait: "background".into(),
        ..pending(3 * 86_400)
    };
    assert!(!asking.asks_on_its_own(NOW) && asking.will_ask_again(NOW));
    assert_eq!(asking.badge(NOW), None);
    assert_eq!(asking.attention(NOW), Attention::Quiet);
    let first_stop = Row {
        phase: Phase::Failed("signal-refused".into()),
        stop_streak: 1,
        streak_why: "signal-refused".into(),
        wait: "failed".into(),
        ..pending(3 * 86_400)
    };
    assert!(!first_stop.asks_on_its_own(NOW) && first_stop.will_ask_again(NOW));
    assert_eq!(first_stop.badge(NOW), None);
    // NEGATIVE CONTROL: a stop that repeats, and a person's hold that has
    // stood, do NOT ask again by themselves, at any age.
    let repeating = Row {
        phase: Phase::Failed("no-resume".into()),
        stop_streak: 2,
        streak_why: "no-resume".into(),
        ..first_stop.clone()
    };
    assert!(!repeating.will_ask_again(NOW));
    let held = Row {
        phase: Phase::Announced {
            at_s: NOW - 86_400,
            asks: 1,
        },
        wait: "draft".into(),
        ..pending(3 * 86_400)
    };
    assert!(!held.will_ask_again(NOW));
    // A limit ends by itself: a record however far behind.
    let limited = Row {
        wait: "limited".into(),
        ..asking.clone()
    };
    assert!(limited.asks_on_its_own(NOW));
    assert_eq!(limited.badge(NOW), None);
    assert_eq!(limited.attention(NOW), Attention::Quiet);
    // NEGATIVE CONTROL: the same cycle a few hours behind is the upgrade
    // working — and the view's next wake is the instant the clock runs out.
    let young = Row {
        behind_since: NOW - 8 * 3_600,
        ..gave_up
    };
    assert!(young.asks_on_its_own(NOW));
    assert_eq!(young.badge(NOW), None);
    assert_eq!(young.attention(NOW), Attention::Quiet);
    assert_eq!(
        next_change(std::slice::from_ref(&young), NOW),
        Some(young.behind_since + MOVE_BUDGET_S)
    );
    let turned = young.behind_since + MOVE_BUDGET_S;
    assert!(!young.asks_on_its_own(turned));
    assert_eq!(young.badge(turned), None, "the day is news, not a mark");
    assert!(
        matches!(young.attention(turned), Attention::Digest(n) if n.kind == "own-work"),
        "{:?}",
        young.attention(turned)
    );
}

/// A LIMIT WHOSE NAMED RESET HAS PASSED IS TOLD (design record 2026-09-28,
/// §3.2 C6, `Wall(until)`; `HarnessUpgradeProgress`'s `LimitRecordForEver`): a
/// tab left at its usage limit's notice after the reset the transcript named
/// has no turn to clear it, the loop's wall guard withholding every point
/// while the notice stands. The record keeps the reset (`limit_until`), and
/// [`LIMIT_GRACE_S`] past it the move is told
/// whatever its age, its phase or the owner's `--now` (which never waives a
/// limit) — AS ATERM'S OWN FAULT, ONE LINE IN THE LOG (`limit-passed`): the
/// resume that continues a session at its reset has not cleared the notice,
/// which the owner can do nothing about now. Never a row, a mark or a digest
/// line (an alert persists only where the owner can act); the view wakes for
/// that instant. NEGATIVE CONTROLS (ruling 307): a reset still ahead, a reset
/// inside its grace, and a record with no reset read (`0`) are a record at any
/// age, unmarked and quiet.
#[test]
fn a_limit_whose_named_reset_has_passed_is_told() {
    let passed = NOW - LIMIT_GRACE_S - 3 * 3_600;
    let at = |phase: Phase, behind_for: u64, limit_until: u64| Row {
        phase,
        wait: "limited".into(),
        limit_until,
        ..pending(behind_for)
    };
    let announced = Phase::Announced {
        at_s: NOW - 600,
        asks: 2,
    };
    let rested = Phase::Failed(upgrade::GAVE_UP.into());
    for phase in [Phase::Pending, announced.clone(), rested.clone()] {
        // A day and more behind, and an hour: told either way.
        for behind_for in [MOVE_BUDGET_S + 3_600, 3_600] {
            let row = at(phase.clone(), behind_for, passed);
            assert!(row.limit_passed(NOW), "{phase:?}");
            assert!(!row.asks_on_its_own(NOW), "{phase:?}/{behind_for}");
            assert_eq!(row.stall(NOW).as_deref(), Some("overdue"), "{phase:?}");
            // A fault line in the log, once: never a mark, a row or an
            // interrupt.
            assert_eq!(row.badge(NOW), None, "{phase:?}/{behind_for}: no mark");
            assert_eq!(row.interrupt(NOW), None);
            let Attention::Fault(need) = row.attention(NOW) else {
                panic!("{phase:?}/{behind_for}: {:?}", row.attention(NOW));
            };
            assert_eq!(need.kind, "limit-passed");
            assert!(
                need.why.contains("reset passed 4 h ago")
                    && need.why.contains("still read the limit"),
                "{need:?}"
            );
            assert!(need.why.chars().count() <= NEED_WHY_MAX, "{need:?}");
            assert!(need.then.chars().count() <= NEED_THEN_MAX, "{need:?}");
            assert!(
                !need.why.contains("paused by a usage limit"),
                "never the limit itself: {need:?}"
            );
            // The row's words say it passed, and when; never "at a usage
            // limit".
            let words = row.stall_words(NOW).expect("stalled");
            assert!(
                words.contains("its usage limit's reset passed 4 h ago"),
                "{words}"
            );
            assert!(!words.contains("it is at a usage limit"), "{words}");
            let line = row.line(NOW);
            assert!(
                line.contains(" cause=limit-passed told=limit-passed"),
                "{line}"
            );
        }
    }
    // The owner's `--now` waives no limit, and quiets none past its reset.
    let hurried = Row {
        request: Request::Now,
        request_at: NOW - 60,
        ..at(Phase::Pending, 3_600, passed)
    };
    assert!(
        matches!(hurried.attention(NOW), Attention::Fault(_)),
        "the word does not hide it"
    );

    // Negative controls: a limit is a record until its reset and grace at any
    // age, as is one whose reset no look read.
    for limit_until in [0, NOW + 3_600, NOW - LIMIT_GRACE_S + 60] {
        for phase in [Phase::Pending, announced.clone(), rested.clone()] {
            let row = at(phase.clone(), 3 * 86_400, limit_until);
            assert!(!row.limit_passed(NOW), "{limit_until}");
            assert!(row.asks_on_its_own(NOW), "{limit_until}/{phase:?}");
            assert_eq!(row.badge(NOW), None, "{limit_until}/{phase:?}");
            assert_eq!(
                row.attention(NOW),
                Attention::Quiet,
                "{limit_until}/{phase:?}"
            );
            assert!(
                row.line(NOW).contains(" cause=limit told=-"),
                "{}",
                row.line(NOW)
            );
        }
    }
    // A reset named but not the recorded wait (the model answered since, the
    // screen moved on): nothing of the limit's is told.
    let moved_on = Row {
        wait: "not-idle:busy".into(),
        limit_until: passed,
        ..pending(3_600)
    };
    assert!(!moved_on.limit_passed(NOW) && moved_on.attention(NOW) == Attention::Quiet);

    // THE VIEW WAKES FOR THE INSTANT: a reset still ahead is named, its grace
    // past it, and the class turns there — a second before, quiet.
    let ahead = at(Phase::Pending, 3_600, NOW + 600);
    let turns = NOW + 600 + LIMIT_GRACE_S;
    assert_eq!(
        next_change(std::slice::from_ref(&ahead), NOW),
        Some(turns),
        "named"
    );
    assert_eq!(ahead.attention(turns - 1), Attention::Quiet);
    assert!(matches!(ahead.attention(turns), Attention::Fault(_)));
    assert_eq!(ahead.next_attention_at(NOW), Some(turns));
    // With no reset read, the reset's instant is never named; the BOUND is
    // (a week after the wait began, `LONGEST_LIMIT_S`): quiet a second
    // before it, a fault at it.
    let unnamed = Row {
        limit_until: 0,
        ..ahead.clone()
    };
    assert_eq!(
        next_change(std::slice::from_ref(&unnamed), NOW),
        Some(unnamed.behind_since + STALLED_AFTER_S),
        "the age's rung first, which a limit's record does not turn on"
    );
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

    // The record keeps the reset the transcript named, through its file.
    let st = St {
        phase: Phase::Pending,
        to: "2.1.283".into(),
        wait: "limited".into(),
        pending_since: NOW - 3_600,
        limit_until: passed,
        ..St::default()
    };
    let back = St::from_json(&st.to_json()).expect("parses");
    assert_eq!(back.limit_until, passed);
    let row = Row::of("s", &back, NOW);
    assert_eq!(row.limit_until, passed);
    assert!(matches!(row.attention(NOW), Attention::Fault(_)));
    assert_eq!(
        row.to_json(NOW).get("limit_until").and_then(Value::as_u64),
        Some(passed)
    );
}

/// WHAT HOLDS THE MOVE AND WHAT THE OWNER IS TOLD, ON `--status` (design record
/// 2026-09-28, §3.2 C8): `cause=` names the class of the wait — for a record as
/// for a row — and `told=` whether the owner has been told and of what (`-`:
/// nothing told; else the KIND of the fault line, digest line or interrupt,
/// [`Row::attention`]). `--json` carries both as `cause` and `told` (and the
/// class as a sentence, `cause_words`). A wait is a record: told only where the
/// owner's rule says.
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
    // Its own work: a record an hour behind, and past six hours — still a
    // record (a timer is not an inability).
    assert_eq!(words(&pending(3_600)), pair("own-work", "-"));
    assert_eq!(words(&pending(STALLED_AFTER_S + 60)), pair("own-work", "-"));
    // The same work held for a day: one digest line.
    assert_eq!(
        words(&pending(MOVE_BUDGET_S + 60)),
        pair("own-work", "own-work")
    );
    // A person's box, past six hours: a record.
    let boxed = Row {
        wait: "box".into(),
        ..pending(STALLED_AFTER_S + 60)
    };
    assert_eq!(words(&boxed), pair("person", "-"));
    // Aterm's own refused notice: told as aterm's fault in the log, whatever
    // runs under the agent.
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
    // A stop that has not repeated is a record; a move done tells nothing.
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
    // A wait this build does not classify is named as such.
    let odd = Row {
        wait: "something-new".into(),
        ..pending(3_600)
    };
    assert_eq!(words(&odd), pair("unclassified", "-"));
}

/// The window reads a `--now` refused for a stopped round as typed words;
/// the CLI prints only the sentence.
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

// ---------------------------------------------------------------- what the owner is told

/// EVERY WAIT WORD A CLAUDE CODE OR CODEX STEP CAN RECORD, for the sweeps of
/// the attention tests below.
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
    "background-terminal",
    "daemon-first:busy-thread",
    "daemon-first:background-terminal",
    "daemon-first:owner-held",
    "daemon-first:unseen-client",
    "daemon-first:clients-unreadable",
    "daemon-first:attended",
    "daemon-first:held",
    "daemon-first:update-failed",
    "daemon-first:vendor-ahead",
    "daemon-first:behind",
    "left-typed-backoff",
    "left-typed:x",
    "no-shell-integration",
    "thread-locked",
    // The Codex lane's ladder and goal pause (rulings 380 and 381), and the
    // waiting record's words.
    "goal",
    "goal:switch",
    "goal-held",
    "goal-pausing",
    "goal-pause-refused",
    "goal-resume",
    "goal-resuming",
    "daemon-turn",
    "daemon-busy",
    "no-daemon",
    "daemon-version",
    "daemon-env",
    "pin",
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
    "tab-not-live",
    "no-socket",
    "conversation-in-other-tab",
];

/// HOW LOUD AN UPGRADE IS IS DECIDED BY WHETHER THE OWNER CAN DO ANYTHING,
/// NEVER BY HOW LONG THE SESSION HAS BEEN BEHIND — AND NO WAIT IS QUIET FOR
/// EVER (the owner: "don't continue to persist alerts to the user that are not
/// actionable"). For every wait word, in either phase, for either agent: a record
/// an hour old is quiet (bar the two owner-only facts that need no time); the
/// same record with no progress in a hundred days is NEVER quiet and never an
/// interrupt — a digest line where the owner has the one action that ends it,
/// otherwise a fault line in the log, one, quoting the last waits.
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
                    // The wait itself began a minute ago: only the record's
                    // age moves.
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
                    // The classes that need the owner name the one action.
                    Attention::Digest(n) => {
                        assert!(!n.then.is_empty() && !n.why.is_empty(), "{at}");
                    }
                }
            }
        }
    }
}

/// A WAIT NO CLASS CLAIMS TURNS A FAULT LINE AT ITS OWN HORIZON, never
/// silence, and a notice typed or a round begun is progress: a person's box,
/// login, hold or attendance and a settle stand a day (the supervisor answers
/// a box, the person ends the rest), a usage limit a week and a day, notices
/// unread behind a full queue three days; the instant is named
/// ([`Row::next_attention_at`]) so the host looks then. A relaunched agent
/// that reaches no idle point is one after [`RELAUNCH_STUCK_S`]. NEGATIVE
/// CONTROLS: a row that names no record (one read off Claude's live status)
/// has no start to count from, and a notice just typed keeps a hundred-day-old
/// record quiet.
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
        assert!(n.why.contains("No progress in"), "{}", n.why);
        assert!(
            n.log_line.starts_with("upgrade need=no-progress tab="),
            "{}",
            n.log_line
        );
        assert_eq!(
            row(wait, bound - 60).next_attention_at(NOW),
            Some(NOW + 60),
            "{wait}: the host wakes at the horizon"
        );
    }
    // A notice typed is progress: the horizon counts from it.
    let asked = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        noticed_at: NOW - 600,
        ..row("box", 100 * 86_400)
    };
    assert_eq!(asked.attention(NOW), Attention::Quiet);
    // A relaunched agent that never reaches an idle point.
    let relaunched = |ago: u64| Row {
        phase: Phase::Relaunched { at_s: NOW - ago },
        ..pending(60)
    };
    assert_eq!(
        relaunched(RELAUNCH_STUCK_S - 60).attention(NOW),
        Attention::Quiet
    );
    assert!(matches!(
        relaunched(RELAUNCH_STUCK_S + 60).attention(NOW),
        Attention::Fault(ref n) if n.kind == "relaunch-stuck"
    ));
    // NEGATIVE CONTROL: a row that names no record.
    let live = Row {
        minted_at: 0,
        ..row("box", 100 * 86_400)
    };
    assert_eq!(live.attention(NOW), Attention::Quiet);
    assert_eq!(live.next_attention_at(NOW), None);
}

/// THE AGENT'S OWN WORK IS THE ROUND'S, NOT THE LAST LOOK'S: each notice,
/// give-up and re-arm clears the wait and the list of holders, so the class
/// must not fall to quiet until the next look (each flip a new log line and a
/// dismissed entry back). The last look that saw the work is remembered for
/// [`OWN_WORK_HOLD_S`], across the acts and across the box or draft a look met
/// in between; a limit is its own class, and the memory runs out.
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
    // The look that saw it.
    let looked = Row {
        wait: "failed".into(),
        held_by: vec![zsh],
        ..round.clone()
    };
    assert!(digest(&looked));
    // Right after an act: no wait, no holders — and the same class.
    let acted = Row {
        wait: String::new(),
        ..round.clone()
    };
    assert!(digest(&acted), "an act is no end of the work");
    // A box or a draft a look met between two looks at it.
    for wait in ["box", "draft", "login", "held", "attended"] {
        let between = Row {
            wait: wait.into(),
            ..round.clone()
        };
        assert!(digest(&between), "{wait}: between two looks at the work");
    }
    // NEGATIVE CONTROLS: a limit is its own class, and the memory runs out.
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
    // The instant the memory runs out is named.
    assert_eq!(
        acted.next_attention_at(NOW),
        Some(NOW - 60 + OWN_WORK_HOLD_S),
        "the host wakes when the memory runs out"
    );
}

/// THE ONLY INTERRUPT IS AN AGENT THE UPGRADE ENDED AND THAT IS NOT BACK: a
/// move that stopped after the agent was seen gone (`agent-down`, the
/// resume command one action away), a shutdown that outlived three times its
/// bound (`hung-exit`; nothing harder is ever sent), an agent gone with its
/// resume line not typed past the bound. NEGATIVE CONTROLS: every other stop,
/// a restart within its bounds, a refusal, a give-up, a relaunched agent that
/// has not reached an idle point (the supervisor's box), a finished move and
/// the owner's word are not.
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
    }
    // The interrupt's one action is the command that takes the conversation
    // back: named in the sentence, carried whole in `command` (never cut into
    // the sentence, which is capped: the launch's flags make it long), and
    // appended by every surface that says the action.
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
    assert_eq!(need.then, "Resume it in that tab with this command");
    assert_eq!(need.fingerprint, format!("agent-down/{TAB}"));
}

/// A WHOLE ROUND NEVER FLIPS THE OWNER'S LINE AT ITS ACTS: one round — a
/// resting look, the rest, the re-arm, four notices with a look either side of
/// each, the give-up, the release, three resting looks — where every act clears
/// the wait and the holders the last look recorded. Driven through the record's own transitions
/// (`note_step`, `note_held`, `announced`, `give_up`, `released`, `rearm`) and
/// read after each with the real classifier: the line stands, unbroken, from
/// the first look. NEGATIVE CONTROL: with the work never seen (no holders, a
/// status that is no work) the same round is quiet throughout.
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
        // The rest: the host looks at a resting round about every ten minutes
        // (the ladder's last rung), two hours to the re-arm.
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
    // NEGATIVE CONTROL: work never seen - a person's box at every look, and
    // nothing under the agent: the same round says nothing of the agent's own
    // work.
    let unseen = round(false);
    assert!(
        unseen.iter().all(|c| c != "digest:own-work"),
        "no work seen, no work line: {unseen:?}"
    );
}

/// EVERY INTERRUPT IS A STALL THE HOST POSTS: the window's host keys its band
/// row on `Row::stall` and takes the shape from the class
/// (`upgrade_host::post_upgrade_stalls`), so an interrupt with no stall word
/// would be a row nothing posts. The agent gone (a stop after its exit, an
/// exit stale past its bound) and the shutdown that hung, for either agent.
#[test]
fn every_interrupt_is_a_stall_the_host_posts() {
    let stale = super::super::STALE_S;
    for agent in [upgrade::Agent::Claude, upgrade::Agent::Codex] {
        for (why, row) in [
            (
                "stopped after the exit",
                Row {
                    phase: Phase::Failed("stale-exit".into()),
                    exited_at: NOW - 600,
                    ..pending(60)
                },
            ),
            (
                "no resume",
                Row {
                    phase: Phase::Failed("no-resume".into()),
                    exited_at: NOW - 600,
                    ..pending(60)
                },
            ),
            (
                "gone, its line not typed",
                Row {
                    phase: Phase::Exiting {
                        at_s: NOW - stale - 60,
                    },
                    exited_at: NOW - stale - 30,
                    ..pending(60)
                },
            ),
            (
                "hung",
                Row {
                    phase: Phase::Exiting {
                        at_s: NOW - HUNG_EXIT_AFTER_S - 60,
                    },
                    ..pending(60)
                },
            ),
            (
                "relaunched, never at a prompt",
                Row {
                    phase: Phase::Relaunched {
                        at_s: NOW - RELAUNCH_INTERRUPT_S - 60,
                    },
                    exited_at: NOW - RELAUNCH_INTERRUPT_S - 120,
                    ..pending(60)
                },
            ),
        ] {
            let row = Row { agent, ..row };
            assert!(row.interrupt(NOW).is_some(), "{agent:?} {why}");
            assert!(row.stall(NOW).is_some(), "{agent:?} {why}: no stall word");
            // …and never the `overdue` word: the one stall the window's host
            // may group into a single row for several tabs
            // (`upgrade_host::post_upgrade_stalls`) - an interrupt is per tab.
            assert_ne!(
                row.stall(NOW).as_deref(),
                Some("overdue"),
                "{agent:?} {why}: an interrupt is never an overdue row"
            );
        }
    }
}

/// THE AGENT'S OWN WORK IS NEWS ONLY AFTER A DAY AND A ROUND: quiet within the
/// first day, quiet with no whole round
/// of notices answered "not yet", ONE digest line past both — and the words
/// carry the counts, never a pid. The day is the PROCESS's ([`Row::behind_since`]): a record that
/// outlived a restart no longer lends its age
/// (`an_age_belongs_to_one_process`).
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
    // A resting round IS the agent's own work outlasting the notices, with
    // or without the processes recorded; the words
    // say only what is known.
    let bare_row = round(30 * 3_600, 2, false);
    let bare = digest(&bare_row).expect("a round given up on");
    assert_eq!(
        bare.why,
        "aterm never ends running work, and it is busy with work of its own"
    );
    assert_eq!(bare.then, "Ask it to wrap up in the tab when it suits you");
    assert_eq!(
        bare_row.more_lines(&bare, NOW)[0],
        "Asked 8 times in 1d6h, unanswered"
    );
    let news_row = round(DIGEST_AFTER_S, 2, true);
    let news = digest(&news_row).expect("a day, a round, work");
    assert_eq!((news.kind, news.severity), ("own-work", NeedSeverity::Info));
    // THE REASON THE OWNER IS NEEDED is aterm's rule; what runs and the
    // notices are the details (`more_lines`), and no pid is ever in a
    // sentence.
    assert_eq!(
        news.why,
        "aterm never ends running work, and work it started is still running"
    );
    assert_eq!(news.then, "Stop that work in the tab when it suits you");
    let more = news_row.more_lines(&news, NOW);
    assert_eq!(more[0], "Asked 8 times in 1d, unanswered", "{more:?}");
    assert!(more[1].starts_with("Held by "), "{more:?}");
    assert!(more.len() <= 3 && more.iter().all(|l| l.chars().count() <= 200));
    assert!(!news.why.contains("pid") && !news.then.contains("pid"));
    assert!(
        news.log_line.contains("held by pid 4000"),
        "{}",
        news.log_line
    );
    // An ANNOUNCED round counts its own notices so far.
    let asking = Row {
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 2,
        },
        wait: "background".into(),
        ..round(30 * 3_600, 1, true)
    };
    let news = digest(&asking).expect("announced, a round behind it");
    assert_eq!(
        asking.more_lines(&news, NOW)[0],
        "Asked 6 times in 1d6h, unanswered"
    );
}

/// A RECORD AN OLDER BUILD WROTE IS AGED FROM ITS MINTING UNTIL A VISIT SETTLES
/// IT: it names no process and keeps no wait log, and its age predates its own
/// minting, so it is read from the minting, the safe direction (a later
/// digest, never an earlier one). NEGATIVE CONTROLS: every record this
/// build writes names its process or keeps a wait log, and is read as it
/// stands; so is an age from within the record's own life.
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

/// THE INTERRUPT'S MARK NEVER CUTS THE ACTION: with a long model name in the
/// move's words, the head — the move and why — gives way, with an ellipsis,
/// before the 200-byte cut reaches the resume command's conversation id.
#[test]
fn the_interrupt_mark_never_cuts_the_action() {
    let session = "07422bc3-1be9-4875-9672-2eaf7b28aa21";
    for model in [
        "",
        "opus",
        "claude-opus-5-5-20260901",
        "claude-sonnet-4-5-20250929-with-a-long-name-that-goes-on-and-on",
    ] {
        let down = Row {
            phase: Phase::Failed("stale-exit".into()),
            exited_at: NOW - 600,
            session: session.to_string(),
            model: model.to_string(),
            ..pending(7 * 3_600)
        };
        let mark = down.badge(NOW).expect("the agent is down");
        assert!(mark.len() <= 200, "{model}: {} bytes", mark.len());
        assert!(
            mark.ends_with(&format!(
                "Resume it in that tab with this command: claude --resume {session}"
            )),
            "{model}: {mark}"
        );
        assert!(
            mark.contains("could not start it again") || mark.contains('…'),
            "{mark}"
        );
    }
}

/// THE HOST IS TOLD THE INSTANT A ROW READS OTHERWISE BY TIME ALONE, FOR EVERY
/// ROW, so the log line and the digest entry never come late: read off
/// [`Row::attention`] itself — every span its classifier compares an age
/// against, added to every instant it compares it from — never a second copy
/// of its horizons. Here: for a grid of rows the instant named is an instant
/// the class really differs at, and no scanned instant before it does.
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
                    for (ago, remembered) in [(3_600, 0), (30 * 3_600, NOW - 600), (9 * 86_400, 0)]
                    {
                        let row = Row {
                            agent,
                            phase: phase.clone(),
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
                        // The first scanned instant the class differs at.
                        let first = (1..=horizon / step)
                            .map(|k| NOW + k * step)
                            .find(|t| row.attention(*t).level() != here);
                        let at = format!("{agent:?} {phase:?} wait={wait:?} held={held} ago={ago}");
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

/// THE FIRST LOOK OF A PROCESS LOWERS WHAT AN EARLIER ONE RAISED: a seamless
/// update carries `owner=upgrade` across the handoff while the successor's
/// view starts with nothing marked. One
/// idempotent `meta unset attention owner=upgrade` per live tab the look
/// wants no mark on, sent once; a tab it does mark is marked, not lowered.
#[cfg(unix)]
#[test]
fn a_first_look_lowers_a_mark_an_earlier_process_raised() {
    let dir = ScratchDir::new("upgrade-status", "carried-mark");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("stale-exit".to_string()),
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: now_s() - 60,
            exited_at: now_s() - 600,
            ..St::default()
        },
    );
    let tabs = [
        LiveTab {
            sid: TAB.to_string(),
            fgpgid: Some(7),
        },
        LiveTab {
            sid: "s-other".to_string(),
            fgpgid: Some(8),
        },
    ];
    let nobody = |_: &BTreeSet<String>| -> Option<Vec<Holder>> { Some(Vec::new()) };
    let mut view = View::default();
    let meta = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
            .unwrap_or_default()
    };
    let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    let first = meta(&asked);
    assert_eq!(first.len(), 2, "{first:?}");
    assert_eq!(
        first[0], "@s-other meta unset attention owner=upgrade",
        "the tab no mark is wanted on is lowered, first"
    );
    assert!(
        first[1].starts_with(&format!("@{TAB} meta set attention owner=upgrade ")),
        "and the agent that is down is marked, not lowered: {first:?}"
    );
    let _ = view.refresh_with(&o, &tabs, now_s(), &nobody, &mut |_| {});
    assert_eq!(meta(&asked), first, "the sweep is once, the mark is once");
}

/// A DIGEST LINE IS KEYED BY KIND AND TAB, NEVER BY THE BUILD, AN AGE OR THE
/// WORDS (Claude ships about daily, so a build key would raise again what the
/// owner dismissed): the same
/// tab's `no-terminal` line has one fingerprint for 2.1.284 and for 2.1.290,
/// however old, and another tab's has its own. Every sentence fits its cap
/// and asks for no restart (the reporters' guard reads the same words).
#[test]
fn a_digest_line_is_keyed_by_kind_and_tab_and_says_no_restart() {
    let needle = ["restart", "relaunch", "reopen", "next launch", "reboot"];
    let mut kinds = std::collections::BTreeSet::new();
    let mut faults = std::collections::BTreeSet::new();
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
                for text in [&na.why, &na.then, &na.title.to_string()] {
                    for n in needle {
                        assert!(!text.to_lowercase().contains(n), "{n} in {text}");
                    }
                }
                // The band's title law, with the place the host adds
                // (`in tab 12`): six words (a bare number is no word), 48
                // characters, no seam, no trailing period.
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
                // The exceptions engine's rule for a `Need` — ONE action, one
                // line (it refuses `; `, ` or ` and a newline): only a fault,
                // which no one is asked to act on, may say two things.
                assert!(!na.why.contains('\n') && !na.then.contains('\n'));
                if !fault {
                    assert!(
                        !na.then.contains("; ") && !na.then.contains(" or "),
                        "{}: {}",
                        na.kind,
                        na.then
                    );
                }
                if fault {
                    faults.insert(na.kind);
                } else {
                    kinds.insert(na.kind);
                }
            }
        }
    }
    assert_eq!(
        kinds,
        std::collections::BTreeSet::from([
            "agent-down",
            "daemon-busy",
            "hung-exit",
            "no-integration",
            "no-terminal",
            "other-client",
            "own-work",
            "refused-launch",
            "repeating-stop",
            "suspended",
            "typed-draft",
        ]),
        "every kind the sweep reaches"
    );
    assert_eq!(
        faults,
        std::collections::BTreeSet::from(["daemon-update-failing", "harness-stuck"]),
        "every fault the sweep reaches"
    );
}

/// A ROW THAT CROSSES A HORIZON IS A CHANGED ROW (the level a row carries,
/// [`Level`]): a wait that turns an hour old is a digest line from that
/// instant, and the window is told THEN — not at some later change. Without
/// the carried level the rows before and after the instant were equal, the
/// host woke at the instant [`next_change`] names, and nothing was sent.
/// NEGATIVE CONTROL: a wait that stays quiet crosses the same instants and
/// sends nothing.
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
            // Nothing crosses a horizon of the classifier's before the
            // stall's six hours (`STALLED_AFTER_S`, which still ages the
            // `upgrade=` column and the words).
            // (or the record's watch, [`super::super::watch_at`], if that
            // comes first: it is the host's wake, not a class change)
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

/// ATERM'S OWN FAULT IS A RECORD, NEVER A DIGEST LINE OR A MARK (the owner:
/// "don't continue to persist alerts to the user that are not actionable"): a
/// wait this build does not classify, and a notice the fence keeps refusing,
/// are aterm's to fix. Young they are the upgrade working; 90 minutes on they
/// are ONE Warn line in the log (`Attention::Fault`) and the wait log says the
/// rest. A daemon move that keeps failing is the same after a day. NEGATIVE
/// CONTROLS: the agent's own work, a person, a limit and a suspended job are
/// never a fault, at any age.
#[test]
fn aterms_own_fault_is_one_log_line_never_a_digest_line_or_a_mark() {
    // (The session has been behind for as long as the wait, and at least six
    // hours: what is under test is the wait's own clock.)
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
        // A settle that never settles, an answer awaited past a whole round
        // and a tab aterm cannot reach are aterm's own business too.
        (upgrade::Agent::Claude, "settling"),
        (upgrade::Agent::Claude, "awaiting-ready"),
        (upgrade::Agent::Claude, "tab-not-live"),
        (upgrade::Agent::Codex, "left-typed-backoff"),
    ] {
        let young = at(agent, wait, HARNESS_TELL_S - 60);
        assert_eq!(young.attention(NOW), Attention::Quiet, "{wait}: young");
        let old = at(agent, wait, HARNESS_TELL_S + 60);
        assert!(
            matches!(old.attention(NOW), Attention::Fault(ref n) if n.kind == "harness-stuck"),
            "{wait}: {:?}",
            old.attention(NOW)
        );
        assert_eq!(old.level, Level::Quiet, "(a hand-built row carries none)");
        assert_eq!(old.attention(NOW).level(), Level::Fault("harness-stuck"));
        assert_eq!(old.badge(NOW), None, "{wait}: a fault marks no tab");
        let days = at(agent, wait, 5 * 86_400);
        assert!(matches!(days.attention(NOW), Attention::Fault(_)), "{wait}");
        // The wait's own clock moves the horizon, not the session's age.
        assert_eq!(
            old.next_attention_at(NOW - 3_600),
            Some(old.wait_since + HARNESS_TELL_S),
            "{wait}"
        );
    }
    // A Pending record NO LOOK has recorded a wait for: minted and never
    // visited (or every visit returned before it recorded). Quiet by age, it
    // is told once as aterm's fault.
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
            Attention::Fault(ref n) if n.kind == "harness-stuck" && n.why.contains("No look")
        ),
        "{:?}",
        minted(HARNESS_TELL_S + 60, Phase::Pending, "").attention(NOW)
    );
    assert_eq!(
        minted(HARNESS_TELL_S + 60, Phase::Pending, "").next_attention_at(NOW - 3_600),
        Some(NOW - HARNESS_TELL_S - 60 + HARNESS_TELL_S),
        "the host wakes at the instant it turns a fault"
    );
    // NEGATIVE CONTROLS: an announced round (the notice just went out, the
    // next look is coming), a record that names its wait, and one an older
    // build minted (no instant) are not this fault.
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
    let daemon = |waited: u64| at(upgrade::Agent::Codex, "daemon-first:update-failed", waited);
    assert_eq!(daemon(DIGEST_AFTER_S - 60).attention(NOW), Attention::Quiet);
    assert!(matches!(
        daemon(DIGEST_AFTER_S + 60).attention(NOW),
        Attention::Fault(ref n) if n.kind == "daemon-update-failing"
    ));
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
    // Aterm's own words left typed, however they stand: only a person sends
    // or clears them, so they are a person's hold, never aterm's fault.
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
        // Unless aterm cannot read the tab: its words may be gone already.
        assert_eq!(
            wait_class("left-typed:screen-unreadable"),
            WaitClass::TabUnread
        );
        assert!(!person_holds("left-typed:screen-unreadable"));
    }
}

/// A CODEX DAEMON THREAD THAT HOLDS THE MOVE FOR A DAY IS NEWS (no notice can
/// ask a thread anything, so there is no round to wait for): a digest line
/// with the one action that ends it — a thread is stopped (`codex agents`
/// lists them), a background terminal is closed with `/stop`. Younger it is
/// the upgrade waiting for the daemon's own work.
#[test]
fn a_codex_daemon_thread_is_news_after_a_day() {
    let row = |wait: &str, waited: u64| Row {
        agent: upgrade::Agent::Codex,
        wait: wait.to_string(),
        wait_since: NOW - waited,
        ..pending(100 * 86_400)
    };
    for wait in [
        "daemon-first:busy-thread",
        "daemon-first:background-terminal",
    ] {
        assert_eq!(
            row(wait, DIGEST_AFTER_S - 60).attention(NOW),
            Attention::Quiet,
            "{wait}"
        );
        let Attention::Digest(n) = row(wait, DIGEST_AFTER_S + 60).attention(NOW) else {
            panic!("{wait}");
        };
        assert_eq!(n.kind, "daemon-busy");
        assert_eq!(n.fingerprint, format!("daemon-busy/{TAB}"));
        if wait.ends_with("busy-thread") {
            assert_eq!(n.command.as_deref(), Some("codex agents"));
            assert!(n.then.contains("codex agents"), "{}", n.then);
        } else {
            assert!(n.then.contains("/stop"), "{}", n.then);
        }
    }
}

/// `--status --json` SAYS HOW LOUD THE OWNER IS TOLD AND WHY (the screen's
/// introspection: the class every surface follows is printed where the owner
/// or an agent reads the upgrade): `attention` is `quiet`, `fault`, `digest`
/// or `interrupt`, `attention_kind` names the kind, `cause` the class that
/// holds the move (one word) and `cause_words` the same class as a sentence.
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
    assert_eq!(
        text(&waiting, "cause_words").as_deref(),
        Some("held by its own work")
    );
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

/// AN UPGRADE THAT CANNOT RUN AT ALL (its state folder cannot hold the lock:
/// `busy:state-unwritable`, `busy:lock-unopenable`) has no record and so no
/// row to classify; the HOST files it once, as a digest line, and only the
/// owner can mend a folder's permissions. Its sentence obeys every rule a
/// `Need` does: a reason and ONE action within the caps, no restart word, a
/// glass title within the law, a key that names no tab.
#[test]
fn an_upgrade_that_cannot_run_is_one_owner_sentence() {
    for step in ["busy:state-unwritable", "busy:lock-unopenable"] {
        let need = Need::upgrade_cannot_run(step);
        assert_eq!(need.kind, "upgrade-cannot-run");
        assert_eq!(need.fingerprint, "upgrade-cannot-run/-");
        assert_eq!(need.severity, NeedSeverity::Warn);
        // The step is the log's word, never the person's.
        assert!(!need.why.contains("busy"), "{}", need.why);
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
        assert!(!need.log_line.contains('\n') && need.log_line.contains(step));
    }
    // A step word longer than a sentence can hold is cut in the log line.
    let long = Need::upgrade_cannot_run(&"x".repeat(200));
    assert!(long.log_line.chars().count() < 100, "{}", long.log_line);
}

/// `--status` says the watch's fields after `release=` (only `refused=`,
/// said while there is one, comes after them): when a look off a point read the
/// record, which build, the last point, the guard, and when it is watched
/// next (`due` once past; `-` for none) — design record 2026-09-28, §3.2
/// C8.
#[test]
fn status_says_the_watchs_fields_last() {
    let st = St {
        phase: Phase::Announced {
            at_s: 1_790_531_333,
            asks: 1,
        },
        to: "2.1.283".to_string(),
        tab: "s-3de30c3c66bf5c4496f8".to_string(),
        salt: 1_790_373_043,
        looked_at: 1_790_632_000,
        looked_by: "0.99.0".to_string(),
        point_at: 1_790_600_000,
        guard: "wall".to_string(),
        ..St::default()
    };
    let now = 1_790_632_822;
    let line = Row::of("446a0b3c-1979-414f-ae20-e06e26a2ef44", &st, now).line(now);
    assert!(
        line.ends_with(
            " looked=13m by=0.99.0 point=9h7m guard=wall watch_at=16m cause=unrecorded \
             told=no-progress"
        ),
        "{line}"
    );
    let never = St {
        phase: Phase::Done,
        ..St::default()
    };
    let line = Row::of("446a0b3c-1979-414f-ae20-e06e26a2ef44", &never, now).line(now);
    assert!(
        line.ends_with(" looked=- by=- point=- guard=- watch_at=- cause=- told=-"),
        "{line}"
    );
    // A notice aterm could not type (`refused=`, said only while there is
    // one) is the line's tail, after the watch's fields.
    let refused = St {
        refused: 2,
        refused_at: now - 600,
        ..st
    };
    let line = Row::of("446a0b3c-1979-414f-ae20-e06e26a2ef44", &refused, now).line(now);
    assert!(
        line.ends_with(" guard=wall watch_at=16m cause=unrecorded told=no-progress refused=2/10m"),
        "{line}"
    );
}

/// A NOTICE STOPPED PART-WAY IS NEVER "NOTHING TO DO": the host stops a pressed
/// notice at a person's key, a screen switch or its deadline — or types it
/// whole and its guarded Enter misses — presses no Enter, and the part typed
/// sits in the prompt (`announce-refused:cut-<why>`). The owner reads that
/// aterm stopped it, unsent, and that the part left waits in the prompt to be
/// sent or cleared: aterm never takes it back. What stands of it is ATERM'S
/// (`left-typed:<why>`), never read as a person's draft — but only a person
/// moves it, nothing of aterm's will: a person's hold however it stands — alone
/// and unsent, behind a box or no drawn composer, or with a person's keys among
/// it (`left-typed:mixed`). NEGATIVE CONTROLS: a notice its fence refused whole
/// typed nothing, and is tried again — nothing to do; a person's own draft is a
/// person's hold.
#[test]
fn a_notice_stopped_part_way_names_the_part_left_in_the_prompt() {
    for why in ["person", "changed", "deadline", "guard"] {
        let wait = format!("announce-refused:cut-{why}");
        assert_eq!(
            claude_wait_words(&wait),
            Some("aterm stopped its notice part-way, unsent"),
            "{wait}"
        );
        let line = pending_with(&wait).waiting_line("09:00");
        assert!(
            line.contains("the part it left in its prompt waits to be sent or cleared there")
                && !line.contains("nothing to do")
                && !line.contains("take"),
            "{wait}: {line}"
        );
    }
    for why in ["unsent", "box", "no-composer"] {
        let wait = format!("left-typed:{why}");
        assert_eq!(
            claude_wait_words(&wait),
            Some("aterm's own words are still in its prompt, unsent"),
            "{wait}"
        );
        let row = pending_with(&wait);
        let line = row.waiting_line("09:00");
        assert!(
            line.contains("it moves once they are sent or cleared in its tab")
                && !line.contains("take"),
            "{wait}: {line}"
        );
        assert_eq!(wait_class(&wait), WaitClass::Person, "{wait}");
        assert!(person_holds(&wait), "{wait}");
    }
    let mixed = "left-typed:mixed";
    assert_eq!(
        claude_wait_words(mixed),
        Some("aterm's words are in its prompt, with a person's keys")
    );
    assert!(
        pending_with(mixed)
            .waiting_line("09:00")
            .contains("that draft is sent or cleared")
    );
    assert_eq!(wait_class(mixed), WaitClass::Person);
    assert!(person_holds(mixed));
    // Negative controls.
    let line = pending_with("announce-refused:changed").waiting_line("09:00");
    assert!(line.contains("nothing to do"), "{line}");
    assert_eq!(wait_class("draft"), WaitClass::Person);
    assert_eq!(
        claude_wait_words("draft"),
        Some("a draft waits in its prompt")
    );
}

/// A Codex lane's own words left unsent are a person's to clear: the user reads
/// that the harness's text stands unsent and the move goes once it is cleared,
/// never "sent" (a sent `/exit` ends Codex with no relaunch) and never lifted by
/// `--now`, nor "nothing to do" while a goal is held paused. Negative controls:
/// with a person's keys among it (`mixed`), their draft; a goal held with no
/// words left, the goal's own "nothing to do".
#[test]
fn a_codex_lanes_own_words_left_unsent_are_a_person_to_clear() {
    let row = |wait: &str| Row {
        agent: upgrade::Agent::Codex,
        wait: wait.to_string(),
        ..pending(7 * 3_600)
    };
    let unsent = row("left-typed:unsent");
    let line = unsent.waiting_line("09:00");
    assert!(
        line.contains("the harness's own text is still in its composer, unsent")
            && line.contains("it moves once that text is cleared in its tab")
            && !line.contains("beside a person's typing")
            && !line.contains("sent or cleared"),
        "{line}"
    );
    assert_eq!(wait_class("left-typed:unsent"), WaitClass::Person);
    assert_ne!(unsent.remedy(NOW), Some(Remedy::Now));
    let goal_held = |wait: &str| Row {
        goal_held_since: NOW - 600,
        ..row(wait)
    };
    let line = goal_held("left-typed:unsent").waiting_line("09:00");
    assert!(
        line.contains("it moves once that text is cleared in its tab")
            && !line.contains("nothing to do"),
        "{line}"
    );
    assert!(
        goal_held("goal:paused")
            .waiting_line("09:00")
            .ends_with("nothing to do"),
        "NEGATIVE CONTROL"
    );
    // Negative control.
    let line = row("left-typed:mixed").waiting_line("09:00");
    assert!(
        line.contains("beside a person's typing") && line.contains("that draft is sent or cleared"),
        "{line}"
    );
}

/// aterm's own notice that its fence would not type is told as such: the row
/// names aterm's refusal, how often and for how long, and the mark names the
/// cause; a refusal standing `TELL_S` is told before six hours behind.
/// `Upgrade now` is offered only where not already in force: under `--now` it
/// would ask for the very notice aterm cannot type, so the row waits, and the
/// word restarts the count (`St::new_round`). Negative controls: a refusal
/// within `TELL_S` under six hours behind is no stall; no refusal counted
/// keeps the old words; with no word in force `Upgrade now` is offered.
#[test]
fn a_notice_aterm_could_not_type_is_told_as_aterms_own() {
    // The word's instant, and a look hours later.
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
    // Negative control: no word in force: `Upgrade now` is offered.
    let unasked = Row {
        request: Request::None,
        request_at: 0,
        ..a.clone()
    };
    assert_eq!(unasked.remedy(now), Some(Remedy::Now));
    let words = a.stall_words(now).expect("stalled");
    assert!(
        words.contains(
            "aterm could not type its notice: the screen moved as it typed (3 times over 5 h)"
        ),
        "{words}"
    );
    // ATERM'S OWN FAILURE IS A RECORD, NEVER A MARK:
    // the words above are the record's and `--status`'s; the tab's diamond,
    // the menu-bar row and the notification are for an agent that is down.
    assert_eq!(a.badge(now), None, "aterm's own failure marks no tab");
    assert!(
        matches!(a.attention(now), Attention::Fault(ref n) if n.kind == "harness-stuck"
            && n.why.contains("refused 3 times")),
        "{:?}",
        a.attention(now)
    );
    assert!(a.line(now).ends_with(" refused=3/5h39m"), "{}", a.line(now));

    // Told past TELL_S, whatever its age and the owner's word: two hours
    // behind, refused for an hour and a half.
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
    // The fault counts from the FIRST refusal of the streak, whatever the
    // refusal's reason has been since (`changed`, then `yield`).
    let switched = Row {
        wait: "announce-refused:yield".to_string(),
        wait_since: now - 60,
        ..young.clone()
    };
    assert!(matches!(switched.attention(now), Attention::Fault(_)));
    // NEGATIVE CONTROLS: within TELL_S it is not told yet; nothing counted,
    // nothing told.
    let fresh = Row {
        refused_at: now - TELL_S + 60,
        ..young.clone()
    };
    assert_eq!(fresh.stall(now), None);
    assert_eq!(fresh.attention(now), Attention::Quiet);
    let uncounted = Row {
        refused: 0,
        refused_at: 0,
        ..young
    };
    assert_eq!(uncounted.stall(now), None);

    // The user's word restarts the count (`St::new_round`): the row it leaves
    // is no stall, so a pressed `Upgrade now` is not followed by the same row.
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

/// A round re-armed after a give-up, still held by its agent's own work, is a
/// record until its next notice goes, like the announced and resting rounds
/// around it. Negative controls: a first round waiting on the same work is a
/// row; a re-armed round waiting on a person is a row; one whose notice aterm
/// could not type past `TELL_S` is told.
#[test]
fn a_rearmed_round_its_agents_own_work_holds_is_a_record() {
    let rearmed = |wait: &str| Row {
        last_stop: upgrade::GAVE_UP.to_string(),
        ..pending_with(wait)
    };
    for wait in ["not-idle:shell", "not-idle:busy", "busy", "settling"] {
        let row = rearmed(wait);
        assert_eq!(row.stall(NOW).as_deref(), Some("overdue"), "{wait}");
        assert!(row.asks_on_its_own(NOW), "{wait}: a record");
        assert_eq!(row.badge(NOW), None, "{wait}: no mark");
    }
    // The passing gate words, while shells run under the agent.
    let held = Row {
        held_by: vec![HeldBy {
            pid: 60_041,
            name: "zsh".to_string(),
            since: NOW - 3_600,
        }],
        ..rearmed("attended")
    };
    assert!(held.asks_on_its_own(NOW));
    // Negative controls.
    assert!(
        !pending_with("not-idle:shell").asks_on_its_own(NOW),
        "a first round"
    );
    assert!(!rearmed("draft").asks_on_its_own(NOW), "a person's draft");
    let refused = Row {
        refused: 4,
        refused_at: NOW - TELL_S,
        ..rearmed("announce-refused:changed")
    };
    assert!(!refused.asks_on_its_own(NOW), "aterm's own failure: told");
}

// ---------------------------------------------------------------- repeated
// asks, relaunches, refusals

/// A SESSION ASKED AGAIN AND AGAIN IS NEWS: a session asked at the end of every
/// run of its work must not read quiet for ever. A whole round is
/// [`upgrade::MAX_ASKS`] declined notices, given up on or not. NEGATIVE
/// CONTROLS: fewer asks in a young round, a day not yet behind, and no work
/// under the agent stay quiet.
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
    // The reviewer's probe: the ninth ask of a hundred-day-old round.
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
    // And asking again does not make it "progress": the backstop counts an ask
    // past a round's own as none.
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
    // NEGATIVE CONTROLS.
    assert_eq!(
        asked(upgrade::MAX_ASKS - 1, 30 * day).attention(NOW),
        Attention::Quiet
    );
    assert_eq!(
        asked(9, 20 * 3_600).attention(NOW),
        Attention::Quiet,
        "not yet a day"
    );
    // A person's box is a person's, however many notices were asked.
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
    // The same session after ONE give-up is news as it always was.
    let gave_up = Row {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 1,
        wait: "failed".into(),
        ..asked(9, 100 * day)
    };
    assert!(matches!(gave_up.attention(NOW), Attention::Digest(_)));
}

/// A RELAUNCHED AGENT THAT REACHES NO PROMPT IS A LOG LINE, THEN AN INTERRUPT:
/// it came back on a screen that is neither a box (the supervisor answers those
/// within moments) nor idle — the same fact as an agent that did not come back:
/// only the owner can look at the tab. A fault line at
/// [`RELAUNCH_STUCK_S`], the owner interrupted at [`RELAUNCH_INTERRUPT_S`]
/// with the one action, and no interrupt for a relaunch that reached its
/// prompt.
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
        assert_eq!(
            need.why,
            "It came back after the upgrade and has not reached a prompt"
        );
        assert_eq!(need.then, "Look at that tab and answer what it shows");
        assert!(need.command.is_none());
        assert_eq!(need.fingerprint, format!("relaunch-stuck/{TAB}"));
        assert!(
            row.badge(NOW)
                .is_some_and(|b| b.len() <= 200 && b.ends_with(&need.then))
        );
    }
    // NEGATIVE CONTROL: the owner's own word holds it, as it holds any.
    let held = Row {
        request: Request::Skip("2.1.284".into()),
        ..relaunched(3_600)
    };
    assert_eq!(held.attention(NOW), Attention::Quiet);
}

/// LOOKS AT THE SAME WORK DO NOT CHANGE THE ROW: the row carries `own_work_at`,
/// so restamping it at every look would send the window a summary, and wake its
/// GUI thread, about every minute. The stamp is renewed at most every quarter
/// of the hold it feeds ([`OWN_WORK_RESTAMP_S`]).
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
    // Thirty looks, a minute apart, over half an hour: the stamp is renewed
    // once (at the restamp interval), not at every look.
    assert!(changes <= 2, "{changes} changes over 30 looks");
    assert!(changes >= 1 || rows[0] == rows[29]);
    // NEGATIVE CONTROL: a look a restamp interval later is a changed row (the
    // remembered work must not run out while it is seen).
    let later = t0 + 60 * 30 + super::super::OWN_WORK_RESTAMP_S;
    st.note_step("wait:background", "busy", later);
    st.note_held("wait:background", &held(later), later);
    assert_eq!(st.own_work_at, later);
}

/// A NOTICE A PERSON OR A PEER REFUSED IS NOT ATERM'S TO FIX:
/// `announce-refused:held` (another driver's lease), `:draft` and `:box` are
/// the fence telling the truth, never a Fault naming aterm's screen. A refusal
/// that names a person's state stands until they end it: the day's backstop,
/// and a draft is news at a day as any draft is. NEGATIVE CONTROL: the refusal
/// aterm's own screen causes (`changed`) is aterm's, at 90 minutes.
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
    // A day on: a peer's lease or a box is a backstop line; a draft is news.
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
    // NEGATIVE CONTROL: aterm's own screen refusing (`changed`) is aterm's fault.
    let own = refused("changed", 2 * 3_600);
    assert!(
        matches!(own.attention(NOW), Attention::Fault(n) if n.kind == "harness-stuck"),
        "{:?}",
        own.attention(NOW).level()
    );
}

/// THE MANUAL RESUME KEEPS THE LAUNCH'S FLAGS UNDER EVERY SHAPE OF THE RELAUNCH LINE
/// the harness itself builds ([`upgrade::relaunch_line_env`]): with and without the
/// survey export, with and without the subshell's `cd`, under zsh, bash and fish
/// (which has no `cd` form). A change to the line's shape that hides the program from
/// [`resume_command`] fails here, not on the owner's tab 1.
///
/// AND IT IS THE ONE LINE: the one every other place tells for the
/// launch ([`crate::harness::upgrade_drive::resume_command_kept`]), each word quoted
/// as it quotes it — read by a rule of its own, a 1M model's `[1m]` or a directory
/// with a space dropped every flag — and the whole of it on the tab's mark, within its
/// 200 bytes. NEGATIVE CONTROL: a quote in a flag's value, which fish writes with a
/// backslash no blind read of the line takes exactly, gives the bare line under fish.
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

/// A SESSION THE HARNESS RELAUNCHED UNDER A LOGIN OF ITS CHOOSING IS RESUMED
/// BY HAND UNDER THE SHELL'S, AND THE REMEDY SAYS SO IN ONE CLAUSE (the
/// accounts design, §6; `resume::SHELL_LOGIN_CLAUSE` has the reasons): the
/// command carries the launch's flags and NO environment — a login directory
/// would be the first thing a 200-byte mark drops — and the sentence that
/// hands it over names which login it runs under. Read off the record's own
/// relaunch line under every shape the harness builds, a directory with a
/// space and an apostrophe and the empty value (the default login, named)
/// alike; each sentence still fits its cap. NEGATIVE CONTROL: a line that
/// carries no login — every line of a machine with no roster — leaves each
/// sentence exactly as it was.
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
    // The sentences, whole: the clause is the only difference.
    let moved = format!(" {LOGIN_ENV}='' '/opt/claude' '--resume' '{session}'");
    let plain = format!(" '/opt/claude' '--resume' '{session}'");
    for (why, sentence) in [
        ("stale-exit", "Resume it in that tab with this command"),
        (
            "shell-gone",
            "Open a new tab and resume it there with this command",
        ),
    ] {
        assert_eq!(
            down(why, &moved).interrupt(NOW).expect("agent-down").then,
            format!("{sentence}, under that shell's own Claude login")
        );
        assert_eq!(
            down(why, &plain).interrupt(NOW).expect("agent-down").then,
            sentence
        );
    }
    // A variable that only LOOKS like the login — a flag's value, another
    // name — is none; nor is one on an earlier command of the line.
    for line in [
        format!(" '/opt/claude' '--append-system-prompt' '{LOGIN_ENV}=/x' '--resume' '{session}'"),
        format!(" CLAUDE_CONFIG_DIR='/x' '/opt/claude' '--resume' '{session}'"),
        format!(" {LOGIN_ENV}=/x true; '/opt/claude' '--resume' '{session}'"),
        String::new(),
    ] {
        assert_eq!(resume_login(&line), None, "{line}");
    }
    // A Codex has no such login, whatever its record's line holds.
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
    assert_eq!(
        crate::harness::resume::login_clause(Some("")),
        SHELL_LOGIN_CLAUSE
    );
    assert_eq!(crate::harness::resume::login_clause(None), "");
}

/// THE INTERRUPT'S ACTION FOLLOWS FROM WHY THE AGENT COULD NOT BE BROUGHT BACK,
/// AND CARRIES THE LAUNCH'S OWN FLAGS: a tab whose shell is gone has no prompt
/// to type at, a Codex whose rollout is gone has nothing to resume, and a bare
/// `claude --resume <id>` would drop `--dangerously-skip-permissions`. The
/// command is the record's own planned line's flags and the conversation, in
/// `Need::command`, never cut into the capped sentence. NEGATIVE CONTROLS: a
/// line whose flags would not leave the mark its head room, another program's,
/// one with no `--resume`, or no line at all carries none.
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
    // The flags are kept.
    let n = need(&down("stale-exit", line));
    assert_eq!(n.then, "Resume it in that tab with this command");
    assert_eq!(
        n.command.as_deref(),
        Some(&*format!(
            "claude --dangerously-skip-permissions --resume {session}"
        ))
    );
    // Every after-exit reason names its own action.
    assert_eq!(
        need(&down("shell-gone", line)).then,
        "Open a new tab and resume it there with this command"
    );
    for why in ["no-resume", "relaunch-refused", "exited-before-continuing"] {
        assert_eq!(
            need(&down(why, line)).then,
            "Resume it in that tab with this command",
            "{why}"
        );
    }
    // The mark says the command whole, whatever the head has to give up.
    let mark = down("stale-exit", line).badge(NOW).expect("marked");
    assert!(
        mark.len() <= 200
            && mark.ends_with(&format!(
                "claude --dangerously-skip-permissions --resume {session}"
            )),
        "{mark}"
    );
    // THE WHOLE LINE — its sentence, the login clause and the flags — IS
    // BUDGETED AGAINST THE MARK, AND THE PERMISSION MODE IS THE LAST THING IT
    // GIVES UP: a fixed flag allowance blind to the sentence beside it would
    // cut the conversation id off a mark whose sentence carries the login
    // clause, and drop every flag at once past it.
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
    for (flags, login, why, then, want) in [
        // 78 bytes of flags under the login clause: the mode alone.
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' '/x/src'",
            true,
            "stale-exit",
            "Resume it in that tab with this command, under that shell's own Claude login",
            skip.as_str(),
        ),
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' '/x/src'",
            true,
            "shell-gone",
            "Open a new tab and resume it there with this command, under that shell's own Claude \
             login",
            skip.as_str(),
        ),
        // Past the allowance, no login: the mode alone, never the bare line.
        (
            "'--dangerously-skip-permissions' '--model' 'claude-opus-5-5[1m]' '--add-dir' \
             '/Users//x/src'",
            false,
            "stale-exit",
            "Resume it in that tab with this command",
            skip.as_str(),
        ),
        // A mode the mark holds only without the login clause: the clause
        // gives way first.
        (
            "'--allow-dangerously-skip-permissions' '--permission-mode' 'plan' \
             '--append-system-prompt' 'You orchestrate this repo'",
            true,
            "shell-gone",
            "Open a new tab and resume it there with this command",
            plan.as_str(),
        ),
    ] {
        let row = down(why, &launched(flags, login));
        let n = need(&row);
        assert_eq!(
            (n.then.as_str(), n.command.as_deref()),
            (then, Some(want)),
            "{why}: {flags}"
        );
        let mark = row.badge(NOW).expect("marked");
        assert!(mark.len() <= 200 && mark.ends_with(want), "{why}: {mark}");
    }
    // Codex: `codex resume`, and none where the rollout is gone.
    let codex = |why: &str| Row {
        agent: upgrade::Agent::Codex,
        ..down(why, "")
    };
    assert_eq!(
        need(&codex("no-resume-hint")).command.as_deref(),
        Some("codex resume")
    );
    let gone = need(&codex("no-rollout"));
    assert_eq!(
        (gone.then.as_str(), gone.command),
        ("Start Codex again in that tab", None)
    );
    // NEGATIVE CONTROLS.
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

/// THE OWN-WORK LINE NAMES WHAT KEEPS THE AGENT BUSY, IN WORDS THE DOOR
/// TAKES: a `/goal` re-prompts Claude at every turn end, its status stays busy,
/// and stopping the programs alone does not open the restart's gate. Every arm
/// — programs under it, a goal, both, a break of its own background work,
/// none of these — is handed to the exceptions door the window hands it to
/// (`aterm_messages::exceptions::Entry::message`): a reason that leans on
/// "waiting" or an action with an " or " in it is refused there, logged and
/// never shown (2026-10-08: the background arm's "waiting on background work
/// of its own", twelve refusals in a day, the entry never on screen).
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
    // The words of `row`'s own-work line, through the door.
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
        // A goal in force is named, in the reason and in the action.
        assert_eq!(why.contains("/goal"), *goal, "{at}");
        assert_eq!(then.contains("/goal"), *goal, "{at}");
        // A break of the agent's own background work is named as one.
        let breaking = wait.contains("background") && *held == 0 && !*goal;
        assert_eq!(why.contains("background"), breaking, "{at}");
    }
    // The arms say different things: programs, a goal, both, a break, the
    // agent's own busy turn.
    let mut whys: Vec<&str> = [0, 2, 3, 5, 8]
        .iter()
        .map(|i| said[*i].0.as_str())
        .collect();
    whys.sort_unstable();
    whys.dedup();
    assert_eq!(whys.len(), 5, "{said:?}");
}

/// THE OWN-WORK LINE DOES NOT MOVE WITH THE HOLDER COUNT: what runs under
/// the agent comes and goes at every look, and a count in the reason changed
/// the entry's words with it — 44 restatements in a day on the owner's
/// machine (2026-10-08). Two looks that differ only in how many programs run
/// under the agent say one line, with or without a goal; the count is the
/// details' (`more_lines`), and those still say it.
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

/// THE ASK COUNT IS THE PROCESS'S, NOT THE ROUND'S (2026-10-08: a tab whose
/// notice the ledger records 22 times read "Asked 4 times in 9d4h and
/// declined"): counted from the round alone — whole rounds given up and this
/// round's asks — a retarget that started a round over lost every ask before
/// it. Asked twice, retargeted onto a round of its own (the model rule named
/// another model: a new consent), asked four more times and given up, the
/// record read back from its state file says six, as unanswered.
/// NEGATIVE CONTROL: another process holding the conversation (the owner
/// restarted it by hand) is asked afresh, from none.
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
    // What holds the move is the agent's own work.
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
    assert_eq!(more[0], "Asked 6 times in 9d, unanswered", "{more:?}");
    // NEGATIVE CONTROL: the conversation held by another process.
    let other = super::super::Proc {
        start: "Fri Oct  2 09:00:00 2026",
        started_s: NOW - 60,
    };
    let anew = St::for_target(Some(st), &running, &build("2.1.285"), None, other, NOW);
    assert_eq!(anew.notices, 0, "{anew:?}");
}

/// A CODEX GOAL THE MOVE PAUSED AND COULD NOT RESUME IS THE UPGRADE'S OWN
/// DAMAGE (ruling 381: aterm never leaves the goal paused): an interrupt with
/// the one hand step in the tab, whatever became of the move and whatever the
/// owner's word on it says. Its thread fallen into a sandbox names the relaunch
/// first. NEGATIVE CONTROL: with nothing left paused, the same finished move is
/// quiet.
#[test]
fn a_codex_goal_left_paused_is_an_interrupt_whatever_the_owners_word_says() {
    let paused = Row {
        agent: upgrade::Agent::Codex,
        phase: Phase::Done,
        done_at: NOW - 60,
        goal_left_since: NOW - 600,
        ..pending(3_600)
    };
    let Attention::Interrupt(need) = paused.attention(NOW) else {
        panic!("{:?}", paused.attention(NOW));
    };
    assert_eq!(need.kind, "goal-paused");
    assert_eq!(need.then, "Type /goal resume in that tab");
    assert_eq!(need.since, NOW - 600);
    assert!(
        paused
            .badge(NOW)
            .is_some_and(|m| m.contains("/goal resume")),
        "{:?}",
        paused.badge(NOW)
    );
    // The owner's `--skip` or `--defer` does not reach a paused goal.
    for request in [
        Request::Skip("2.1.282".to_string()),
        Request::DeferUntil(NOW + 3_600),
    ] {
        let held = Row {
            request,
            ..paused.clone()
        };
        assert!(held.interrupt(NOW).is_some(), "{:?}", held.request);
    }
    let sandboxed = Row {
        wait: "goal-sandboxed".to_string(),
        ..paused.clone()
    };
    let need = sandboxed.interrupt(NOW).expect("an interrupt");
    assert_eq!(need.title, "Goal left paused");
    assert!(
        need.then.starts_with("Quit Codex in that tab"),
        "{}",
        need.then
    );
    for need in [paused.interrupt(NOW), sandboxed.interrupt(NOW)]
        .into_iter()
        .flatten()
    {
        assert!(need.why.chars().count() <= NEED_WHY_MAX, "{}", need.why);
        assert!(need.then.chars().count() <= NEED_THEN_MAX, "{}", need.then);
        let words = format!("{} {}", need.why, need.then).to_lowercase();
        for banned in ["restart", "relaunch", "reopen", "next launch", "reboot"] {
            assert!(!words.contains(banned), "{banned}: {words}");
        }
    }
    let resumed = Row {
        goal_left_since: 0,
        ..paused
    };
    assert_eq!(resumed.attention(NOW), Attention::Quiet);
}

/// NO UPGRADE STUCK FOREVER, bound to the reducer and the view (design record
/// 2026-09-28, §4.4): the Tier-1 binds of `HarnessUpgradeProgress`.
#[path = "upgrade_progress_tests.rs"]
mod progress;

/// A round re-armed after a give-up is its rest until a look says otherwise:
/// the re-arm clears the recorded wait and the processes under the agent, so
/// the round's first look (nothing recorded, a person at the tab, a draft)
/// must not read as a new cause; each is asking on its own, no mark
/// (`rest_carries_on`). Negative controls: the same words in a first round are
/// a row; a person's hold standing a whole re-ask is a row; a wait `Upgrade
/// now` moves is a row (`a_rearmed_round_its_agents_own_work_holds_is_a_record`).
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
    // Negative controls.
    for wait in ["", "attended", "draft"] {
        assert!(
            !pending_with(wait).asks_on_its_own(NOW),
            "{wait:?}: a first round is a row"
        );
    }
    let stood = rearmed("draft", NOW - upgrade::REASK_S);
    assert!(!stood.asks_on_its_own(NOW), "a draft that stood: a row");
    // The tab is marked for an interrupt only: a
    // draft that stood is not the rest, but a wait is no mark.
    assert_eq!(stood.badge(NOW), None, "no mark for a wait");
    let queued = rearmed("queued", NOW - 60);
    assert!(
        !queued.asks_on_its_own(NOW),
        "`Upgrade now` moves it: a row"
    );
}

/// What time alone changes reaches the window: the host sends a summary only
/// when it differs, so a time-driven change must change the row at an instant
/// the view names: a row stall reaches its next age word (`behind for 7 h` →
/// `8 h`), a re-armed round's draft standing a whole re-ask stops being its
/// rest, and the move clock turns a record into a row. A record whose words
/// state its age (shown in Settings ▸ Messages) reaches its next word too.
/// Negative control: read at the same second nothing differs (and
/// `a_stall_whose_words_carry_no_age_never_wakes_for_it`).
#[test]
fn what_time_alone_changes_is_a_changed_row_at_a_named_instant() {
    let read = |mut row: Row, now: u64| {
        row.read_at(now);
        row
    };
    // An overdue row's age, by the hour past its first hour.
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
    // A record states its age too: the next hour, never a minute, and read an
    // hour on it differs.
    let rearmed = |wait: &str| Row {
        last_stop: upgrade::GAVE_UP.to_string(),
        wait_since: NOW - 60,
        ..pending_with(wait)
    };
    let resting = read(rearmed("not-idle:shell"), NOW);
    assert!(resting.on_its_own);
    assert!(
        resting
            .stall_words(NOW)
            .is_some_and(|w| w.starts_with("behind for 7 h")),
        "{:?}",
        resting.stall_words(NOW)
    );
    assert_eq!(resting.behind_said.as_deref(), Some("7 h"));
    assert_eq!(
        next_change(std::slice::from_ref(&resting), NOW),
        Some(NOW + 3_600)
    );
    let resting_later = read(rearmed("not-idle:shell"), NOW + 3_600);
    assert!(resting_later.on_its_own, "still a record an hour on");
    assert_eq!(resting_later.behind_said.as_deref(), Some("8 h"));
    assert_ne!(resting_later, resting, "an hour on: sent again");
    // Half an hour on, the next word is still the hour's, never a minute's.
    let half = NOW + 1_800;
    assert_eq!(
        next_change(
            std::slice::from_ref(&read(rearmed("not-idle:shell"), half)),
            half
        ),
        Some(NOW + 3_600)
    );
    // The move clock: a record turned a row is a changed row.
    let past = read(rearmed("not-idle:shell"), NOW - 7 * 3_600 + MOVE_BUDGET_S);
    assert!(!past.on_its_own);
    assert_ne!(past, resting);
    // A re-armed round's draft that has stood.
    let draft = read(rearmed("draft"), NOW);
    assert!(draft.on_its_own, "a draft just typed: its rest");
    let stands = NOW - 60 + upgrade::REASK_S;
    assert_eq!(next_change(std::slice::from_ref(&draft), NOW), Some(stands));
    let stood = read(rearmed("draft"), stands);
    assert!(!stood.on_its_own, "a hold that stood: a row");
    assert_ne!(stood, draft);
}

/// Only a row whose words state its age wakes for it: under an hour
/// [`behind_words`] counts minutes, so a young held-back, failed, refused,
/// stuck or blocked row, whose words ([`stall_reason`]) carry no age, must not
/// wake the host every minute. Only `overdue` says `behind for …`
/// (`Row::age_said`). Negative control: a 7 h overdue row wakes at the next
/// hour.
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
        assert!(
            !first
                .stall_words(NOW)
                .unwrap_or_default()
                .contains("behind for"),
            "{what}: its words carry no age"
        );
        assert_eq!(first.behind_said, None, "{what}");
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
    // Negative control: an overdue row states its age and wakes for it.
    let overdue = read(pending(7 * 3_600), NOW);
    assert_eq!(overdue.stalled.as_deref(), Some("overdue"));
    assert_eq!(overdue.behind_said.as_deref(), Some("7 h"));
    assert_eq!(
        next_change(std::slice::from_ref(&overdue), NOW),
        Some(NOW + 3_600)
    );
    assert_ne!(read(pending(7 * 3_600), NOW + 3_600), overdue);
}

/// AN ANNOUNCED ROUND UNDER THE QUIET FLOW IS JUDGED AS A PENDING ONE: nothing
/// asks it again there — the quiet step
/// answers a round an older aterm announced, and never re-asks nor gives up
/// — so its busy agent's stall reads `overdue` at six hours behind (the
/// column and `--status`), never "asked again every half hour"; either way
/// it is a record, not a mark (the owner's rule: a timer is not an
/// inability).
/// The flow reaches the row from the reader's options (`upgrade_ask`,
/// [`rows`]). NEGATIVE CONTROLS: the same round under the ask flow asks on
/// its own, a record with no mark; a usage limit is a record in either flow.
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
    assert!(quiet.line(NOW).contains(" stalled=overdue "));
    let mut sent = quiet.clone();
    sent.read_at(NOW);
    assert!(!sent.on_its_own, "sent as a row");
    // Negative controls.
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
    // The flow, from the reader's options.
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

/// A STATUS THAT LAGS IS NO COMFORT UNDER THE QUIET FLOW, AND `--now` MOVES
/// IT: the quiet flow types nothing to rewrite a `status-stale` status, so the
/// record says what moves it — Claude's own `idle`, or the owner's word —
/// and the overdue row's remedy is `--now`, which the quiet restart reads the
/// lag past on (`upgrade::quiet_step`). A quiet round that GAVE UP (on its
/// agent's own work, an older aterm's notice) says the next round moves it at
/// a quiet moment, never that it "asks again". NEGATIVE CONTROLS: the ask
/// flow's rows.
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
        let line = quiet.waiting_line("16:00");
        assert!(!line.contains("nothing to do"), "{status}: {line}");
        assert!(line.contains("says it is idle"), "{status}: {line}");
        assert!(line.contains("upgrade now"), "{status}: {line}");
        assert_eq!(
            row(true, STALLED_AFTER_S + 60).remedy(NOW),
            Some(Remedy::Now),
            "{status}"
        );
        // NEGATIVE CONTROLS: the ask flow's notice goes through the lag.
        assert!(row(false, 3_600).waits_for_comfort(), "{status}");
        assert!(
            row(false, 3_600)
                .waiting_line("16:00")
                .contains("nothing to do"),
            "{status}"
        );
        assert_eq!(
            row(false, STALLED_AFTER_S + 60).remedy(NOW),
            Some(Remedy::Waits),
            "{status}"
        );
    }
    // A quiet round that gave up says what comes next as it is.
    let gave_up = |quiet: bool| Row {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        wait: "failed".to_string(),
        quiet,
        ..pending(STALLED_AFTER_S + 3_600)
    };
    let words = gave_up(true).stall_words(NOW).expect("overdue");
    assert!(words.contains("moves it at a quiet moment"), "{words}");
    assert!(!words.contains("asks again"), "{words}");
    let words = gave_up(false).stall_words(NOW).expect("overdue");
    assert!(words.contains("asks again"), "{words}");
}

/// A LIMIT PAST ITS NAMED RESET IS THE QUIET MOVE'S BEFORE IT IS TOLD
/// ([`Row::limit_told_at`]): under the quiet flow a tab whose limit row named
/// its reset may be moved from that reset's grace on, so telling it then would
/// blame aterm for a move about to go. Such a row is told [`QUIET_MOVE_S`] later, and
/// the view wakes for that instant. NEGATIVE CONTROLS: the ask flow's row,
/// and a quiet row whose reset no row named (bounded by the week), are told
/// at the grace's end.
#[test]
fn a_quiet_move_at_a_spent_limit_has_its_chance_before_the_tell() {
    // The reset's grace ends now.
    let reset = NOW - LIMIT_GRACE_S;
    let at = |quiet: bool, limit_until: u64| Row {
        wait: "limited".into(),
        limit_until,
        quiet,
        ..pending(3 * 86_400)
    };
    let quiet = at(true, reset);
    assert!(!quiet.limit_passed(NOW), "the quiet move's chance");
    assert!(quiet.asks_on_its_own(NOW), "a record meanwhile");
    assert!(
        matches!(quiet.attention(NOW), Attention::Quiet),
        "no line while the move may go"
    );
    let told = NOW + QUIET_MOVE_S;
    assert_eq!(
        next_change(std::slice::from_ref(&quiet), NOW),
        Some(told),
        "the view wakes for it"
    );
    assert!(matches!(quiet.attention(told - 1), Attention::Quiet));
    assert!(quiet.limit_passed(told));
    assert!(
        matches!(quiet.attention(told), Attention::Fault(ref n) if n.kind == "limit-passed"),
        "told, in the log, once the chance has passed"
    );
    assert_eq!(quiet.badge(told), None, "and never a mark");
    // NEGATIVE CONTROLS: the ask flow, and a reset no row named.
    let asked = at(false, reset);
    assert!(asked.limit_passed(NOW) && matches!(asked.attention(NOW), Attention::Fault(_)));
    let unnamed = Row {
        wait_since: NOW - LONGEST_LIMIT_S - LIMIT_GRACE_S,
        ..at(true, 0)
    };
    assert!(unnamed.limit_passed(NOW), "the week's bound: no move waits");
    assert!(matches!(unnamed.attention(NOW), Attention::Fault(_)));
}

/// A row served later than it was read is judged with the awake clock as it
/// stands ([`Row::reread`], used by the window's `ls` column and a word's
/// re-apply): a row read before a sleep and served after the wake reads no
/// `unwatched` inside the wake grace, and a row carrying the host's own watch
/// start keeps that grace. Negative control: judged with the clock it was read
/// with, it reads `unwatched`.
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
    // Woken a minute ago.
    pin_awake_since(NOW - 60);
    let mut served = slept.clone();
    served.reread(NOW);
    assert_eq!(served.awake_since, NOW - 60);
    assert_eq!(served.stall(NOW), None, "inside the wake grace");
    assert_eq!(served.stalled, None, "and read so");
    // The host's own grace, later than the machine's clock, is kept.
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

/// A QUIET GIVE-UP ITS AGENT'S OWN WORK HELD IS A WAIT THE OWNER'S `--NOW`
/// DOES NOT MOVE: the quiet flow gives a round up
/// only once the agent's own work has held its restart a whole re-ask window,
/// and the round `--now` re-arms waits on that same work — the quiet
/// restart's gate holds `background` while anything runs under the agent or
/// at a break, whatever the owner said, so [`Remedy::AskAgain`] would offer a
/// button that moves nothing. The work seen since its notice ([`St::work_seen`], carried
/// into the row of a give-up alone) or running under the agent at the last
/// look ([`Row::held_by`]) makes it [`Remedy::Waits`]. NEGATIVE CONTROLS: the
/// ask flow's give-up (`--now` asks it again), a quiet give-up no work held
/// (a status that only lagged: `--now` reads it past), an announced round's
/// work (no give-up: the row carries none of it), and the owner's hold (no
/// remedy at all).
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
    // NEGATIVE CONTROLS.
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

/// A QUIET GIVE-UP THE OWNER'S `--NOW` DOES MOVE KEEPS `UPGRADE NOW`. Two
/// give-ups whose work was seen, where the work's end moves neither and
/// `--now` does:
///
/// - A CODEX give-up. The Codex lane never re-arms a resting round at the
///   end of its TUI's work (its reducer reads no `work_ended`, and a stopped
///   round's visit returns `wait:failed` until its rest is over), so the
///   round waits out [`upgrade::RETRY_S`] whatever the work does. `--now`
///   re-arms it pending, and the quiet restart goes at the first idle point
///   once nothing runs under the TUI.
/// - A CLAUDE CODE give-up whose shells HAVE ENDED, its status still `shell`
///   over an idle screen: the end-of-work re-arm asks the restart's gate,
///   which never reads a lagging status past on its own (`status-stale`), so
///   nothing moves it until its rest is over — while the owner's `--now`
///   reads the lag past (`upgrade::quiet_step`). `work_seen` says work was
///   seen once since the notice, not that it holds the round now.
///
/// Both keep [`Remedy::AskAgain`]. NEGATIVE CONTROLS: the same Claude Code
/// give-up while the shells still run (`held_by`), or resting at a break of
/// its work (`failed`, no end read), stays [`Remedy::Waits`].
#[test]
fn a_quiet_give_up_the_owners_now_does_move_keeps_upgrade_now() {
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.282".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - 3_600,
        failed_at: NOW - 60,
        work_seen: true,
        wait: "failed".to_string(),
        ..St::default()
    };
    let quiet = |st: &St| Row {
        quiet: true,
        ..Row::of("c", st, NOW)
    };
    let codex = quiet(&St {
        agent: upgrade::Agent::Codex,
        ..gave_up.clone()
    });
    assert!(
        codex.work_seen,
        "PRECONDITION: the give-up saw the TUI's work"
    );
    assert!(
        !codex.rests_on_own_work(),
        "no end of work re-arms a Codex round"
    );
    assert_eq!(codex.remedy(NOW), Some(Remedy::AskAgain));
    let lagging = quiet(&St {
        wait: "status-stale:shell".to_string(),
        ..gave_up.clone()
    });
    assert!(lagging.work_seen && lagging.held_by.is_empty());
    assert!(
        !lagging.rests_on_own_work(),
        "the work ended; only the status lags, which `--now` reads past"
    );
    assert_eq!(lagging.remedy(NOW), Some(Remedy::AskAgain));
    // NEGATIVE CONTROLS: what still holds a Claude Code round is its work.
    let running = quiet(&St {
        held_by: vec![HeldBy {
            pid: 4242,
            name: "zsh".to_string(),
            since: NOW - 7_200,
        }],
        ..gave_up.clone()
    });
    assert!(running.rests_on_own_work());
    assert_eq!(running.remedy(NOW), Some(Remedy::Waits));
    let at_a_break = quiet(&gave_up);
    assert!(at_a_break.rests_on_own_work());
    assert_eq!(at_a_break.remedy(NOW), Some(Remedy::Waits));
}

/// A MOVE THE PERSON HOLDS SAYS SO WHERE THEY ARE (the owner: "hey! why didn't
/// you upgrade?" of a move only a keystroke within `KEYS_GAP_S` held). The note
/// names the build, that it is ready and what holds it — the person using the
/// tab, or the draft they leave when they pause — never a promise of when it
/// goes: the same words at the first rung, where anyone within `human_grace_s`
/// holds it, before the READY, and overdue. A row the draft held has words for
/// a draft alone. The looks that only restamp the person move the row, never
/// the note. NEGATIVE CONTROLS: no note once no look has seen the person, for
/// any other wait, once the move is under way, under the owner's holding word,
/// or for a Codex row.
#[test]
fn a_move_the_person_holds_is_noted_for_their_footer() {
    use crate::harness::footer::UpgradeWords;
    let words = |whole: &str, short: &str| UpgradeWords {
        whole: whole.to_string(),
        short: short.to_string(),
    };
    let st = St {
        from: "2.1.285".to_string(),
        to: "2.1.286".to_string(),
        tab: TAB.to_string(),
        pending_since: NOW - (5 * 3_600 + 38 * 60),
        phase: Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        },
        ready_since: NOW - 20,
        wait: "attended".to_string(),
        wait_since: NOW - 20,
        wait_seen: NOW - 5,
        ..St::default()
    };
    let held = Row::of("c", &st, NOW);
    assert!(held.ready && held.person_named, "{held:?}");
    assert_eq!(held.rung(NOW), upgrade::Rung::Land);
    let note = held.pause_note(NOW).expect("held by the person: noted");
    assert_eq!(
        note.using,
        Some(words(
            "Claude Code 2.1.286 is ready \u{b7} waits while you use this tab",
            "2.1.286 waits while you use this tab"
        ))
    );
    assert_eq!(
        note.draft,
        words(
            "Claude Code 2.1.286 is ready \u{b7} waits until your draft is sent or cleared",
            "2.1.286 waits until your draft is sent or cleared"
        )
    );
    assert_eq!(note.words(false), note.using.as_ref());
    assert_eq!(note.words(true), Some(&note.draft));
    // The first rung, where anyone at the tab within `human_grace_s` holds
    // it: the same words — no pause of a keystroke's length is promised.
    let early = Row::of(
        "c",
        &St {
            pending_since: NOW - upgrade::RUNG_KEYS_S / 2,
            ..st.clone()
        },
        NOW,
    );
    assert!(early.rung(NOW) < upgrade::Rung::KeysOnly);
    assert_eq!(early.pause_note(NOW), Some(note.clone()));
    // The person paused with a draft in the composer: the draft holds it, and
    // there are words for a draft alone — an empty composer says it was sent
    // or cleared since.
    let drafted = Row::of(
        "c",
        &St {
            wait: "draft".to_string(),
            ..st.clone()
        },
        NOW,
    );
    assert!(!drafted.person_named);
    assert_eq!(
        drafted.pause_note(NOW),
        Some(crate::harness::footer::UpgradeNote {
            using: None,
            draft: note.draft.clone(),
        })
    );
    // Before the READY: what holds it, the agent not yet agreed.
    let asking = Row::of(
        "c",
        &St {
            ready_since: 0,
            ..st.clone()
        },
        NOW,
    );
    assert!(!asking.ready);
    let asked = asking.pause_note(NOW).expect("held by the person: noted");
    assert_eq!(
        asked.using,
        Some(words(
            "Claude Code 2.1.286 waits while you use this tab",
            "2.1.286 waits while you use this tab"
        ))
    );
    assert_eq!(
        asked.draft.whole,
        "Claude Code 2.1.286 waits until your draft is sent or cleared"
    );
    // Only an announced round's READY is one.
    let pending = Row::of(
        "c",
        &St {
            phase: Phase::Pending,
            ..st.clone()
        },
        NOW,
    );
    assert!(!pending.ready);
    assert_eq!(pending.pause_note(NOW), Some(asked));
    // Overdue is still the person's to move.
    let overdue = Row::of(
        "c",
        &St {
            pending_since: NOW - 7 * 3_600,
            ..st.clone()
        },
        NOW,
    );
    assert_eq!(overdue.stall(NOW).as_deref(), Some("overdue"));
    assert_eq!(overdue.pause_note(NOW), Some(note.clone()));
    // A look that only restamps the person moves the row, not the note.
    let later = Row::of(
        "c",
        &St {
            wait_seen: NOW + 15,
            ..st.clone()
        },
        NOW + 20,
    );
    assert_ne!(later, held, "the row moves at every look");
    assert_eq!(later.pause_note(NOW + 20), Some(note), "its note does not");

    // NEGATIVE CONTROLS.
    let unseen = st.wait_seen + ATTENDED_SEEN_S + 1;
    assert_eq!(Row::of("c", &st, unseen).pause_note(unseen), None, "unseen");
    for wait in [
        "not-idle:busy",
        "box",
        "settling",
        "awaiting-ready",
        "left-typed:mixed",
        "release:draft",
    ] {
        let other = Row::of(
            "c",
            &St {
                wait: wait.to_string(),
                ..st.clone()
            },
            NOW,
        );
        assert_eq!(other.pause_note(NOW), None, "{wait}");
    }
    for phase in [
        Phase::Exiting { at_s: NOW - 1 },
        Phase::Relaunched { at_s: NOW - 1 },
        Phase::Done,
    ] {
        let moved = Row::of(
            "c",
            &St {
                phase: phase.clone(),
                ..st.clone()
            },
            NOW,
        );
        assert_eq!(moved.pause_note(NOW), None, "{phase:?}");
    }
    for request in [
        Request::Skip("2.1.286".to_string()),
        Request::DeferUntil(NOW + 3_600),
    ] {
        let held_back = Row::of(
            "c",
            &St {
                request: request.clone(),
                request_tab: TAB.to_string(),
                ..st.clone()
            },
            NOW,
        );
        assert_eq!(held_back.pause_note(NOW), None, "{request:?}");
    }
    for wait in ["attended", "draft"] {
        let codex = Row::of(
            "codex-c",
            &St {
                agent: upgrade::Agent::Codex,
                wait: wait.to_string(),
                ..st.clone()
            },
            NOW,
        );
        assert_eq!(
            codex.pause_note(NOW),
            None,
            "Claude Code's lane alone: {wait}"
        );
    }
}

/// A word that is a token already is handed back as it is, and reads exactly
/// as the character-at-a-time cut reads it; every near miss takes the cut.
#[test]
fn a_token_word_reads_as_the_cut_reads_it() {
    let cut = |s: &str| {
        let w: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '(' | ')'))
            .take(64)
            .collect();
        if w.is_empty() { "-".to_string() } else { w }
    };
    let long = "a".repeat(64);
    let longer = "b".repeat(65);
    for s in [
        "claude",
        "wait:own-work(3)",
        "0.158.0_x",
        long.as_str(),
        longer.as_str(),
        "",
        "two words",
        "\u{e9}",
        "tab\t",
        "!!",
    ] {
        assert_eq!(word(s), cut(s), "{s:?}");
    }
}

/// A FINISHED MOVE'S ROW CARRIES WHEN IT MOVED (audit 2026-10-09: a move
/// recorded "moved onto 2.1.287" seven days after it happened, at the
/// session's next idle turn end): the record keeps the relaunch's instant
/// through Done (`St::relaunched_at`, which the phase drops), and the row the
/// window is handed names it (`Row::moved_at`), for the record to say when.
/// NEGATIVE CONTROL: a record an older build wrote carries none.
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
