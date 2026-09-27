// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::path::PathBuf;

const NOW: u64 = 1_790_311_076;
const TAB: &str = "s-b5cf2faabac5ce5127bd";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-upgrade-status-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

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
    }
}

/// A live holder of `session` on `version`, proven in `tab`.
fn holder(session: &str, version: &str, tab: Option<&str>) -> Holder {
    Holder {
        session: session.to_string(),
        version: version.to_string(),
        tab: tab.map(str::to_string),
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

/// The measured shape (2026-09-24, 03396a15): native 2.1.281 behind managed
/// 2.1.282 since 13:09, waiting on a busy turn.
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

/// PENDING IS NOT STALLED: a session waiting for its turn end is the upgrade
/// working, and neither the tab nor the band is marked for it. It reads
/// stalled once it is [`STALLED_AFTER_S`] behind, when the agent runs where
/// typing into the tab cannot reach, and when the upgrade stopped — unless
/// the owner's own word holds it.
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
        Some("behind for 8h22m, waiting (not-idle:busy)")
    );
    let tmux = Row {
        wait: "terminal:tmux".to_string(),
        ..pending(60)
    };
    assert_eq!(tmux.stall(NOW).as_deref(), Some("held-back:tmux"));
    for (why, stall) in [
        ("unanswered", "gave-up"),
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
        "stalled/2.1.282/overdue/8h22m"
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
    assert_eq!(
        deferred.column(NOW),
        format!("deferred/2.1.282/defer-until:{}/2m", NOW + 60)
    );
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
    for row in [pending(1), asked, deferred, skipped, restarting, done] {
        let col = row.column(NOW);
        assert!(!col.contains(char::is_whitespace), "{col}");
        assert_eq!(col.split('/').count(), 4, "{col}");
    }
}

/// `--status` reads every state file, filtered to one tab when asked, and
/// carries how long and why.
#[test]
fn status_rows_come_from_the_state_files_and_filter_by_tab() {
    let dir = scratch("rows");
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
             pending_for=8h22m wait=not-idle:busy wait_for=8h20m request=- stalled=overdue"
        )
    );
    let json = only[0].to_json(NOW);
    assert_eq!(
        json.get("pending_for_s").and_then(Value::as_u64),
        Some(30_120)
    );
    assert_eq!(json.get("stalled").and_then(Value::as_str), Some("overdue"));
    let _ = std::fs::remove_dir_all(dir);
}

/// THE OWNER'S WORD lands in the tab's upgrade under the sweep lock, with a
/// ledger line; `--now` re-arms a stopped upgrade; a restart under way and a
/// tab with nothing recorded are refused, and a lock another sweep holds is
/// `busy:another-sweep`, writing nothing.
#[test]
fn an_ask_writes_the_owners_word_under_the_lock_and_refuses_what_it_cannot_hold() {
    let dir = scratch("ask");
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
    let row = ask(&o, TAB, Ask::Now).expect("the ask lands");
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

    let row = ask(&o, TAB, Ask::Skip).expect("skip");
    assert_eq!(row.request, Request::Skip("2.1.282".to_string()));
    let row = ask(&o, TAB, Ask::Defer(3_600)).expect("defer");
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
    let row = ask(&o, TAB, Ask::Now).expect("re-armed");
    assert_eq!(row.phase, Phase::Pending);
    assert!(load(&o, "aaa").expect("state").marker.is_empty());

    // REFUSED or FAILED is not re-armed (review of 2026-09-25: that restored
    // typing, and after READY a SIGTERM, for an upgrade stopped for good):
    // `--now` is refused and nothing is written; `--skip` still ends it.
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
        let e = ask(&o, TAB, Ask::Now).expect_err("not re-armed");
        assert!(e.contains("stopped for good"), "{why}: {e}");
        assert!(e.contains("--skip"), "{why}: {e}");
        assert_eq!(load(&o, "aaa"), Some(stopped), "{why}: nothing written");
        let row = ask(&o, TAB, Ask::Skip).expect("a skip ends it");
        assert_eq!(row.stall(now_s()), None, "{why}");
    }
    save(&o, "aaa", &st);

    // Nothing recorded for another tab: refused, nothing written.
    let e = ask(&o, "s-0000", Ask::Now).expect_err("no upgrade there");
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
    let e = ask(&o, "s-busy", Ask::Defer(60)).expect_err("in flight");
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
    let _ = std::fs::remove_dir_all(dir);
}

/// A WORD FOR THE BUILD THE OWNER WAS SHOWN ([`ask_for`], the window's band
/// row and tab menu): pressed after the upgrade moved on to a newer target,
/// no word lands — not the skip, which would skip a build the owner never
/// saw, nor `Now` or a deferral — and nothing is written, ledgered or
/// announced. NEGATIVE CONTROL: the same words for the target the upgrade
/// moves to now land, as `ask`'s do (`2.1.283.0` is the same build).
#[test]
fn a_word_for_a_target_that_moved_on_lands_on_nothing() {
    let dir = scratch("ask-for");
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
    let _ = std::fs::remove_dir_all(dir);
}

/// THE OWNER'S WORD IS A PUSH (ours: no minute sweep reads it): a word that
/// lands replaces the [`word_marker`] under the harness state — a new file,
/// so the host's activation wake sees its identity change and the tab's
/// worker takes the word at its next idle point. NEGATIVE CONTROL: a word
/// refused (nothing recorded for the tab) replaces nothing.
#[test]
fn an_owners_word_that_lands_wakes_the_window() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = scratch("word-push");
    let o = opts(&dir);
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let marker = word_marker(&o.state);
    assert!(!marker.exists(), "no word yet");
    let e = ask(&o, TAB, Ask::Now).expect_err("nothing recorded");
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
    ask(&o, TAB, Ask::Now).expect("lands");
    let first = std::fs::metadata(&marker)
        .expect("the word announced")
        .ino();
    ask(&o, TAB, Ask::Skip).expect("lands again");
    let second = std::fs::metadata(&marker).expect("announced again").ino();
    assert_ne!(first, second, "a new file each time: its identity changes");
    let _ = std::fs::remove_dir_all(dir);
}

/// THE RELAUNCH'S RECORDS ARE NO UPGRADES: an agent relaunched after an
/// exit, or restarted in place for its memory or its model, files its record
/// beside the upgrade's (`St::cause`), and the owner's view neither shows it
/// as an upgrade of the tab nor lets a word land on it. NEGATIVE CONTROL: the
/// upgrade's own record (no cause) beside it is shown, and is the one a word
/// lands on.
#[test]
fn a_relaunchs_record_is_no_upgrade_of_the_tab() {
    let dir = scratch("relaunch-records");
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
    let e = ask(&o, TAB, Ask::Now).expect_err("no upgrade there");
    assert!(e.contains("no upgrade is recorded"), "{e}");
    save(&o, "zzz", &upgrade);
    let rows = rows_at(&o, NOW);
    assert_eq!(
        rows.iter().map(|r| r.session.as_str()).collect::<Vec<_>>(),
        ["zzz"]
    );
    let row = ask(&o, TAB, Ask::Now).expect("the upgrade's record");
    assert_eq!(row.session, "zzz");
    assert_eq!(load(&o, "exit").expect("kept").request, Request::None);
    let _ = std::fs::remove_dir_all(dir);
}

/// THE VIEW NAMES WHEN IT NEXT CHANGES BY TIME ALONE, so the host looks again
/// then and never on a timer: an upgrade turning overdue, the owner's
/// `--now` no longer quieting it, a `--defer` running out, a finished move
/// leaving the summary — the earliest of them still to come. NEGATIVE
/// CONTROLS: an overdue row with nothing else to come names nothing, and a
/// restart in flight names nothing.
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
    assert_eq!(
        next_change(&[pending(7 * 3_600)], NOW),
        None,
        "already overdue"
    );
    let restarting = Row {
        phase: Phase::Exiting { at_s: NOW },
        ..pending(60)
    };
    assert_eq!(next_change(&[restarting], NOW), None);
    assert_eq!(next_change(&[], NOW), None);
}

/// The host hands the window its tabs' rows only when they CHANGE, and a row
/// that crosses [`STALLED_AFTER_S`] is a change; a tab the roster no longer
/// lists is left out; standing down sends one empty summary.
#[test]
fn the_host_sends_its_tabs_rows_only_when_they_change() {
    let dir = scratch("view");
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
    let _ = std::fs::remove_dir_all(dir);
}

/// The state's new fields survive the file, and a file an older build wrote
/// (no `pending_since`) reads as behind since it was minted.
#[test]
fn the_owners_view_survives_the_state_file_and_an_older_file_reads_its_salt() {
    let st = St {
        phase: Phase::Announced { at_s: 9, asks: 2 },
        to: "2.1.282".to_string(),
        salt: 100,
        pending_since: 50,
        wait: "not-idle:shell".to_string(),
        wait_since: 60,
        request: Request::DeferUntil(700),
        request_tab: TAB.to_string(),
        outcome: "claude restarted on 2.1.282 · model x".to_string(),
        done_at: 800,
        ..St::default()
    };
    assert_eq!(St::from_json(&st.to_json()), Some(st.clone()));
    let older = St {
        pending_since: 0,
        ..st.clone()
    };
    let text = older.to_json().replace(r#""pending_since":0,"#, "");
    let read = St::from_json(&text).expect("an older file");
    assert_eq!(read.behind_since(), 100, "the salt is when it was minted");
    let unknown = st.to_json().replace("defer-until:700", "someday");
    assert_eq!(
        St::from_json(&unknown).expect("still read").request,
        Request::None,
        "an unreadable request asks for nothing"
    );
}

/// ONE STATE FILE, OLDER WRITERS: a file written by the build before the
/// owner's view, by a build that wrote fields this one does not read
/// (`told`, `ready`, the retired board's `yielded`/`yielded_until`) and by the
/// owner's-view build (`request`, `wait`, `pending_since`…) is read by this
/// one, each missing field at its default and an unknown one ignored — and a
/// field this build writes that another reads as unknown changes nothing it
/// reads.
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
    // The retired board's `yielded` and `yielded_until`: ignored.
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
        request: Request::Now,
        request_tab: "s-1".to_string(),
        request_at: 88,
        ..st
    };
    assert_eq!(St::from_json(&both.to_json()), Some(both));
}

/// A control socket at `<dir>/t.sock` (its token beside it) that answers
/// `OK` to every request and records them, standing in for the window's own
/// instance. The accept loop ends with the test process.
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

/// THE TAB'S MARK (gap audit 2026-09-24: both lagging tabs read
/// `attention=-`): the host raises `meta set attention owner=upgrade` on a
/// STALLED tab once, and lowers it when the stall ends. NEGATIVE CONTROL: a
/// tab that is merely waiting for its turn end is never marked.
#[cfg(unix)]
#[test]
fn only_a_stalled_tab_is_marked_and_its_mark_is_lowered_when_it_recovers() {
    let dir = scratch("mark");
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
        phase: Phase::Failed("unanswered".to_string()),
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
    let sweep = |view: &mut View| {
        let _ = view.refresh_with(&o, &tabs, now_s(), &held, &mut |_| {});
    };
    sweep(&mut view);
    assert!(meta(&asked).is_empty(), "waiting is never marked");
    save(&o, "aaa", &stalled);
    sweep(&mut view);
    sweep(&mut view);
    let marked = meta(&asked);
    assert_eq!(marked.len(), 1, "raised once: {marked:?}");
    assert!(
        marked[0].starts_with(&format!(
            "@{TAB} meta set attention owner=upgrade Claude Code"
        )),
        "{marked:?}"
    );
    assert!(marked[0].contains("no READY answer"), "{marked:?}");
    save(
        &o,
        "aaa",
        &St {
            request: Request::Skip("2.1.282".to_string()),
            ..stalled.clone()
        },
    );
    sweep(&mut view);
    let marked = meta(&asked);
    assert_eq!(
        marked.last().map(String::as_str),
        Some(format!("@{TAB} meta unset attention owner=upgrade").as_str()),
        "the owner's skip ends the stall: {marked:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// WHAT A ROW SAYS IT MOVES: a build upgrade names both builds (and the
/// model once the announcement named one); a same-build restart for the
/// priority list's model names the model, never `2.1.282 → 2.1.282`.
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
    assert_eq!(
        same.move_words(),
        "Claude Code 2.1.282 → the priority list's model"
    );
    let named = Row {
        model: "claude-opus-5-5".to_string(),
        ..same
    };
    assert_eq!(named.move_words(), "Claude Code 2.1.282 → claude-opus-5-5");
}

/// WHAT IS SHOWN IS VETTED AGAINST THE LIVE PROCESSES (review of 2026-09-25:
/// a state file outlives what it describes, and was shown for as long as its
/// TAB lived). A pending, announced or stopped upgrade stands only while a
/// live process holds its conversation, IN ITS TAB, on a build OLDER than its
/// target; a finished move and a restart in flight always stand. The host
/// counts no holder whose tab it could not prove; the CLI, with no roster,
/// does not disprove one.
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
    // A SAME-BUILD restart (the priority list's model, the build current)
    // waits on a holder still ON its build — and, like any other, not on one
    // past it or in another tab.
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

/// THE TAB'S MARK IS SENT ONCE PER STALL (review of 2026-09-25: the overdue
/// text carried `behind for 7h1m`, so each minutely sweep re-sent `meta set
/// attention` — three sets in three minutes, each a meta-change event and a
/// wake, and each taking the shown attention back from a later owner). Three
/// sweeps a minute apart on an overdue tab send ONE set, and its text holds no
/// age; a change of KIND is a new text, sent once more.
#[cfg(unix)]
#[test]
fn an_overdue_tab_is_marked_once_not_once_a_minute() {
    let dir = scratch("mark-once");
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
    let marked = sets(&asked);
    assert_eq!(marked.len(), 1, "one set for one stall: {marked:?}");
    assert!(
        marked[0].ends_with("upgrade stalled: behind for more than 6h"),
        "no running age, no wait: {marked:?}"
    );
    // A new KIND of stall is new words: said once more.
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("unanswered".to_string()),
            ..overdue
        },
    );
    view.refresh_with(&o, &tabs, now + 240, &held, &mut |_| {});
    view.refresh_with(&o, &tabs, now + 300, &held, &mut |_| {});
    let marked = sets(&asked);
    assert_eq!(marked.len(), 2, "{marked:?}");
    assert!(marked[1].contains("no READY answer"), "{marked:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// THE OWNER'S `--now` ENDS AN OVERDUE STALL (review of 2026-09-25): a
/// session the owner had already hurried still read `stalled/…/overdue`, the
/// band's row told them to run the `--now` they had run, and the waiting
/// record counted it as staying behind. Under `--now` it is moving: pending,
/// no stall, no remedy, no mark. And the remedy of an overdue stall names
/// `--now` only where `--now` moves it — past the settling window or the
/// attended guard, or at the turn end of a TURN it waits for; one waiting on
/// the READY answer, a draft, a box, a hold, work under the agent — or
/// Claude's status off idle for anything but a turn: a background shell
/// (`not-idle:shell`), a question waiting on a person (`not-idle:waiting`),
/// a status nobody read (`not-idle`), none of which `--now` ends, since it
/// still asks Claude idle — names that wait ([`Remedy::Waits`]; review of
/// 2026-09-25: it offered `--now` for those, and once given the word hid the
/// stall for good). NEGATIVE CONTROLS: without the word it is overdue, and a
/// STOP is still a stall under `--now`.
#[test]
fn an_owners_now_ends_an_overdue_stall_and_its_remedy_is_the_waits() {
    let overdue = pending(7 * 3_600);
    assert_eq!(overdue.stall(NOW).as_deref(), Some("overdue"));
    assert!(overdue.badge(NOW).is_some());
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
        hurried.line(NOW).ends_with(" request=now stalled=-"),
        "{}",
        hurried.line(NOW)
    );
    let stopped = Row {
        phase: Phase::Failed("argv:--print".into()),
        ..hurried
    };
    assert!(stopped.stall(NOW).is_some(), "a stop is still a stall");
    for (wait, remedy) in [
        ("", Remedy::Now),
        ("settling", Remedy::Now),
        ("attended", Remedy::Now),
        ("busy", Remedy::Now),
        ("not-idle:busy", Remedy::Now),
        ("not-idle:shell", Remedy::Waits),
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

/// EVERY OWNER'S WORD ARMS A NEW ROUND (review of 2026-09-25). A hold on an
/// announced upgrade puts it back to pending — and with the salt kept, the
/// notice after the hold asked for the SAME READY marker as the one before
/// it, so a READY given before the word answered a notice typed after it.
/// Now each word mints a new salt past every marker the old one could make:
/// a new round, a new marker. How long the session has been behind is kept.
/// NEGATIVE CONTROL: a step's own state keeps its round (the same target
/// reuses the state as it is).
#[test]
fn every_owners_word_arms_a_new_round() {
    let dir = scratch("round");
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
    ask(&o, TAB, Ask::Defer(600)).expect("defer");
    let held = load(&o, "aaa").expect("state");
    assert_eq!(held.phase, Phase::Pending, "the hold ends the notice");
    assert!(held.marker.is_empty(), "and its marker");
    assert!(held.markers.is_empty(), "every marker of its round");
    // The agent it asked to wind down is released (2026-09-26): one line is
    // owed, typed at the next step that may type.
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
    ask(&o, TAB, Ask::Now).expect("--now");
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
        now_s(),
    );
    assert_eq!(kept.salt, hurried.salt);
    let _ = std::fs::remove_dir_all(dir);
}

/// AN INEFFECTIVE `--now` CANNOT HIDE A STALL FOR GOOD (review of
/// 2026-09-25): `Request::Now` never lapses, and it lowered the band row and
/// the tab's mark for as long as it stood — thirty days on, a session stuck
/// behind a background shell still read `pending … moving`. The word quiets
/// the stall for [`NOW_QUIETS_S`] from when it was given; a session it has
/// not moved by then reads `overdue` again, its remedy the wait it is stuck
/// on. NEGATIVE CONTROL: inside that window the stall stays quiet.
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
    assert!(shell.badge(later).is_some(), "the mark comes back");
    let month = NOW + 30 * 86_400;
    assert_eq!(shell.stall(month).as_deref(), Some("overdue"));
    // A word an older build wrote carries no time: it quiets nothing.
    let untimed = Row {
        request_at: 0,
        ..shell
    };
    assert_eq!(untimed.stall(NOW).as_deref(), Some("overdue"));
}

/// A GAVE-UP UPGRADE RE-ARMED BY `--now` IS NOT STALLED AGAIN AT ONCE (review
/// of 2026-09-25): the band's own remedy for a give-up is `--now`, and a
/// session six hours behind re-armed by it kept its age and read `overdue`
/// at once — the gave-up row resolved, a NEW Warn row naming `--now` again
/// posted, and the tab's mark re-sent under new words, taking the tab's
/// attention back. Now the mark is lowered and nothing new is raised; the
/// summary row reads pending. NEGATIVE CONTROL: the give-up itself is marked.
#[cfg(unix)]
#[test]
fn a_give_up_re_armed_by_now_is_moving_not_stalled_again() {
    let dir = scratch("re-armed");
    let (sock, asked) = recording_socket(&dir);
    let o = Opts {
        sock: Some(sock),
        ..opts(&dir)
    };
    std::fs::create_dir_all(state_dir(&o)).expect("state");
    let now = now_s();
    save(
        &o,
        "aaa",
        &St {
            phase: Phase::Failed("unanswered".to_string()),
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            tab: TAB.to_string(),
            pending_since: now - 7 * 3_600,
            ..St::default()
        },
    );
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    let held = behind_in(TAB);
    let mut view = View::default();
    let mut last = Vec::new();
    view.refresh_with(&o, &tabs, now, &held, &mut |rows| last = rows.to_vec());
    assert_eq!(last[0].stall(now).as_deref(), Some("gave-up"));
    let row = ask(&o, TAB, Ask::Now).expect("--now re-arms a give-up");
    assert_eq!(row.phase, Phase::Pending);
    assert_eq!(row.stall(now), None, "{row:?}");
    view.refresh_with(&o, &tabs, now + 60, &held, &mut |rows| last = rows.to_vec());
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].stall(now + 60), None, "not stalled again");
    let meta: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
        .unwrap_or_default();
    assert_eq!(meta.len(), 2, "{meta:?}");
    assert!(meta[0].contains("no READY answer"), "{meta:?}");
    assert_eq!(
        meta[1],
        format!("@{TAB} meta unset attention owner=upgrade"),
        "lowered, never re-sent: {meta:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A `line:` REFUSAL IS THE RELAUNCH LINE'S (review of 2026-09-25): it read
/// "its launch flags cannot be carried into a resume", which a fish tab in
/// another directory — whose flags are fine — could do nothing with. Each
/// cause is said as what it is. NEGATIVE CONTROL: an `argv:` refusal is
/// still the flags'.
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
    assert!(
        fish.badge(NOW).is_some_and(|b| b.contains("fish")),
        "{:?}",
        fish.badge(NOW)
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

/// The stand-in agent: THIS test binary parked in `harness::upgrade_drive`'s
/// own park test, its tab given in its environment. Not `/bin/sleep`: a
/// platform binary's environment is hidden from `KERN_PROCARGS2`.
#[cfg(unix)]
fn parked_agent(tab: &str) -> std::process::Child {
    let mut child = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args(["harness::upgrade_drive::tests::park_when_asked", "--exact"])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the stand-in agent");
    // Its environment is read once it has exec'd (a fork not yet exec'd reads
    // as this process, with this process's environment).
    for _ in 0..500 {
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

/// NEGATIVE CONTROL THROUGH THE REAL READS (review of 2026-09-25, its scratch
/// test replayed): a `Pending` seven hours old, in a LIVE tab, for a
/// conversation whose live process's session file reports the TARGET build —
/// moved there by hand, which the sweep answers `current` without touching
/// the file — hands the window no row (so no `upgrade=`, no band row, no
/// "moves onto it" count: the window counts only rows handed over) and marks
/// no tab. The same process on the OLDER build is the stall it should be: one
/// row, one mark. Once the conversation ends, the row and the mark go.
#[cfg(unix)]
#[test]
fn a_state_whose_session_is_current_or_gone_shows_nothing() {
    let dir = scratch("phantom");
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
            .map(|a| a.iter().filter(|l| l.contains(" meta ")).cloned().collect())
            .unwrap_or_default()
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
    let current = (sent.clone(), meta(&asked));

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
    let behind = (sent.clone(), meta(&asked));

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
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        current.0,
        vec![Vec::<Row>::new()],
        "a session on its target hands over no row"
    );
    assert!(current.1.is_empty(), "and marks nothing: {:?}", current.1);
    assert_eq!(behind.0.len(), 2, "{:?}", behind.0);
    assert_eq!(behind.0[1].len(), 1);
    assert_eq!(behind.0[1][0].stalled.as_deref(), Some("overdue"));
    assert_eq!(behind.1.len(), 1, "one mark: {:?}", behind.1);
    assert!(
        behind.1[0].starts_with(&format!("@{TAB} meta set attention owner=upgrade")),
        "{:?}",
        behind.1
    );
    assert_eq!(gone.0.len(), 3);
    assert!(
        gone.0[2].is_empty(),
        "an ended conversation hands over none"
    );
    assert_eq!(
        gone.1.last().map(String::as_str),
        Some(format!("@{TAB} meta unset attention owner=upgrade").as_str()),
        "and its mark is lowered: {:?}",
        gone.1
    );
}

/// A STATE AN OLDER BUILD WROTE HAS NO TAB (measured 2026-09-25 on the
/// owner's machine: both live pending states read `"tab":""` — the older
/// build recorded the tab only at the announcement — so `--status` listed
/// nothing for two sessions one and two builds behind, and `--now` could name
/// neither). Such a state is shown under the tab its live holder is PROVEN
/// in, and the owner's word for that tab lands on it and records the tab.
/// NEGATIVE CONTROL: the same state whose holder is proven in ANOTHER tab is
/// neither listed for this tab nor taken by a word for it.
#[cfg(unix)]
#[test]
fn a_state_with_no_recorded_tab_stands_in_the_tab_its_holder_is_in() {
    let dir = scratch("untabbed");
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
    let for_tab = |tab: &str| Opts {
        only_sid: Some(tab.to_string()),
        ..o.clone()
    };
    let other = "s-4906566e7ab0a0c15ee3";

    let mut elsewhere = parked_agent(other);
    register_as(&o.home, elsewhere.id(), session, "2.1.281");
    let (listed_elsewhere, _) = status_rows(&for_tab(TAB));
    let asked_elsewhere = ask(&o, TAB, Ask::Now);
    let _ = elsewhere.kill();
    let _ = elsewhere.wait();
    let untouched = load(&o, session);

    let mut here = parked_agent(TAB);
    register_as(&o.home, here.id(), session, "2.1.281");
    let (listed, vetted) = status_rows(&for_tab(TAB));
    let asked = ask(&o, TAB, Ask::Now);
    let _ = here.kill();
    let _ = here.wait();
    let written = load(&o, session).expect("state");
    let _ = std::fs::remove_dir_all(&dir);

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

/// WHAT THE OWNER CAN DO, BY KIND OF STALL (review of 2026-09-25: the band
/// named `--now` for every kind, and it moves two).
#[test]
fn each_kind_of_stall_names_the_remedy_that_moves_it() {
    let base = pending(60);
    assert_eq!(base.remedy(NOW), None, "not stalled: nothing to do");
    assert_eq!(pending(7 * 3_600).remedy(NOW), Some(Remedy::Now));
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

/// THE CODEX LANE IN THE OWNER'S VIEW: a Codex upgrade's state (`codex-<tab>`)
/// is a row like Claude's — `--status`, the host's `upgrade=` column, and
/// `--now|--defer|--skip` — standing only while the Codex TUI that leads its
/// tab still runs a build older than the target, and said as Codex's.
#[test]
fn a_codex_upgrade_is_listed_held_and_hurried_like_claudes() {
    let dir = scratch("codex");
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
    let held = ask(&o, TAB, Ask::Defer(3_600)).expect("a Codex upgrade to defer");
    assert_eq!(held.agent, upgrade::Agent::Codex);
    assert!(matches!(held.request, Request::DeferUntil(_)));
    assert!(held.column(now_s()).starts_with("deferred/0.157.1/"));
    let skipped = ask(&o, TAB, Ask::Skip).expect("a Codex upgrade to skip");
    assert_eq!(skipped.request, Request::Skip("0.157.1".to_string()));
    let hurried = ask(&o, TAB, Ask::Now).expect("a Codex upgrade to hurry");
    assert_eq!(hurried.request, Request::Now);
    let st = load(&o, &session).expect("the state");
    assert_eq!(st.agent, upgrade::Agent::Codex, "the word keeps the lane");
    assert_eq!(st.request_tab, TAB);
    let _ = std::fs::remove_dir_all(dir);
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
    // The review's repro: every failure a Codex move meets AFTER its `/exit`
    // was vetted against a Codex TUI on the old build in its tab — gone by
    // construction — and dropped from `--status`, the band and `upgrade=`;
    // the owner learned of it from the ledger alone.
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(7),
    }];
    // The HOST'S OWN holder read (the process table, this home's Codex TUIs
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
        let dir = scratch(&format!("cx-failed-{why}"));
        let o = opts(&dir);
        let exited = now_s() - 120;
        codex_state(&o, Phase::Failed(why.to_string()), exited);
        let (shown, vetted) = status_rows(&o);
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
        let _ = std::fs::remove_dir_all(dir);
    }
    // NEGATIVE CONTROL: a failure BEFORE any exit — its TUI never went (a
    // launch whose flags cannot be resumed) — is still vetted against it:
    // with no Codex on the old build in the tab, nothing is shown.
    let dir = scratch("cx-failed-before");
    let o = opts(&dir);
    codex_state(&o, Phase::Failed("argv:unknown-flag".to_string()), 0);
    let mut sent: Vec<Vec<Row>> = Vec::new();
    let mut view = View::default();
    view.refresh_with(&o, &tabs, now_s(), &real(&o), &mut |r| {
        sent.push(r.to_vec());
    });
    assert_eq!(sent.last().map(Vec::len), Some(0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_codex_move_in_flight_refuses_the_owners_word_in_its_own_terms() {
    // No Codex is ever signalled: its move begins with a typed `/exit`.
    let dir = scratch("cx-in-flight");
    let o = opts(&dir);
    codex_state(&o, Phase::Exiting { at_s: now_s() }, 0);
    let err = ask(&o, TAB, Ask::Now).expect_err("a move in flight");
    assert!(err.contains("once its `/exit` is typed"), "{err}");
    assert!(!err.contains("signalled"), "{err}");
    // One that stopped after its `/exit` names what takes it back — nothing
    // is left in the tab to quit — and `--skip` still quiets it.
    codex_state(
        &o,
        Phase::Failed("no-resume-hint".to_string()),
        now_s() - 60,
    );
    let err = ask(&o, TAB, Ask::Now).expect_err("stopped");
    assert!(err.contains("`codex resume` in the tab"), "{err}");
    assert!(!err.contains("quit it"), "{err}");
    let quiet = ask(&o, TAB, Ask::Skip).expect("a skip of the record");
    assert_eq!(quiet.stall(now_s()), None);
    let _ = std::fs::remove_dir_all(dir);
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

/// A WORD HOLDS A GAVE-UP UPGRADE'S LATE READY TOO (2026-09-26). An upgrade
/// that gave up asking still honours a READY to its notices
/// (`upgrade::next_step`), so the owner's `--skip` or `--defer` forgets them:
/// no answer given before the word acts after it. NEGATIVE CONTROL: the record
/// left alone keeps them.
#[test]
fn a_word_on_a_gave_up_upgrade_forgets_its_answers() {
    let dir = scratch("gave-up-word");
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
        ask(&o, TAB, word).expect("the word");
        let held = load(&o, "aaa").expect("state");
        assert!(
            held.marker.is_empty() && held.markers.is_empty(),
            "{word:?}"
        );
        assert_eq!(held.release, "gave-up", "the release still owed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A held-back agent names what it runs under (`tmux`), and one whose terminal
/// owner has no name to give (`terminal:none`, or `terminal:?` read back as
/// `held-back:-`) says it runs under a terminal that is not the tab's, never
/// "runs under none".
#[test]
fn a_held_back_agent_without_an_owner_name_reads_as_a_sentence() {
    assert_eq!(
        stall_reason(upgrade::Agent::Claude, "held-back:tmux"),
        "it runs under tmux, which typing into the tab does not reach"
    );
    for kind in ["held-back:none", "held-back:-"] {
        assert_eq!(
            stall_reason(upgrade::Agent::Claude, kind),
            "it runs under a terminal that is not the tab's, which typing into the tab does \
             not reach",
            "{kind}"
        );
    }
    assert_eq!(runs_under("?"), "a terminal that is not the tab's");
    assert_eq!(runs_under("screen"), "screen");
}
