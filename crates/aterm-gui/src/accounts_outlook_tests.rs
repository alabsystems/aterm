// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the window's accounts outlook ([`super`]): what the memo costs, what the Accounts
//! card maps from its snapshot, and what a forecast never does.

use super::*;
use crate::autopilot_menu::MenuRow;
use crate::claude_footer::probe::{AT_REST, facts, glass_with, pass, publish};
use crate::claude_lights::Fact;
use crate::{App, WindowId};
use aterm_agent::harness::accounts::{Class, Roster, Row, TapRead, WindowFig, WindowRead};
use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::outlook::Figure;
use aterm_agent::harness::profile::SubscriptionDisplay;
use aterm_agent::harness::quota_view::QuotaView;

/// Sun 25 Oct 2026 19:30, UTC.
const NOW: i64 = 1_792_956_600;
const H: i64 = 3600;
const D: i64 = 86_400;

/// The window's reads on the test's thread ([`TEST_STATE_DIR`]) under a fixed policy and UTC.
fn scratch_reads() -> Reads {
    Reads {
        state_dir: LIVE.state_dir,
        policy: || (Mode::Early, false),
        offset: |_| 0,
    }
}

fn set_dir(dir: Option<&Path>) {
    TEST_STATE_DIR.with(|d| *d.borrow_mut() = dir.map(Path::to_path_buf));
}

fn row(label: &str, class: Class) -> Row {
    Row {
        label: label.into(),
        class,
        email: Some(format!("{label}@example.test")),
        ..Row::default()
    }
}

fn roster() -> Roster {
    Roster {
        rows: vec![row("me", Class::Preferred), row("alt1", Class::Preferred)],
    }
}

/// `me`, `alt1` and a never-read `pool1`.
fn pool_roster() -> Roster {
    Roster {
        rows: vec![
            row("me", Class::Preferred),
            row("alt1", Class::Preferred),
            row("pool1", Class::Pool),
        ],
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-outlook-memo-")
        .tempdir()
        .expect("temp dir")
}

fn tap(pct_x10: u32, reset: i64, at: i64, five: bool) -> TapRead {
    TapRead {
        five_hour: five.then_some(WindowRead {
            pct_x10: 380,
            reset: NOW + 2 * H,
            refilled: None,
        }),
        seven_day: Some(WindowRead {
            pct_x10,
            reset,
            refilled: None,
        }),
        at,
        seen_before: true,
        born: None,
        key: None,
    }
}

/// A scratch store whose `me` was read ten minutes ago at 58% of its week.
fn seeded() -> aterm_tempfile::TempDir {
    let dir = scratch();
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(NOW).expect("an empty store loads");
    let at = NOW - 600;
    let _ = accounts::note_reading_held(&mut state, "me", &tap(580, NOW + 3 * D, at, true), at);
    store.save(&state).expect("save");
    set_dir(Some(dir.path()));
    dir
}

/// A scratch pool whose replay projects a stop over [`roster`]: `me` in use at 58% of its week
/// and rising 16 points a day for a day of series, its reset in three days; `alt1` read three
/// hours ago at 96%; [`pool_roster`]'s `pool1` never read.
fn seeded_stop() -> aterm_tempfile::TempDir {
    let dir = scratch();
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(NOW).expect("an empty store loads");
    state.active = Some("me".into());
    let week = NOW + 3 * D;
    let _ = accounts::note_reading_held(&mut state, "me", &tap(580, week, NOW - 600, true), NOW);
    let _ = accounts::note_reading_held(
        &mut state,
        "alt1",
        &tap(960, NOW + 5 * D, NOW - 3 * H, false),
        NOW,
    );
    store.save(&state).expect("save");
    for (hours, pct_x10) in [(24, 420), (16, 470), (8, 520), (0, 580)] {
        let at = NOW - 600 - hours * H;
        let fig = WindowFig {
            pct_x10,
            reset: week,
            at,
            by: state.machine.clone(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        };
        account_series::note(dir.path(), "me", &state.machine, None, Some(&fig), at)
            .expect("series");
    }
    set_dir(Some(dir.path()));
    dir
}

fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).expect("read"));
            }
        }
    }
    out
}

/// T21: THE OUTLOOK IS REPLAYED AT MOST ONCE A MINUTE, on the resolver's thread, and reads
/// the store only when its stamps move. Ten minutes of reads every second (a busy resolver)
/// replay it at most eleven times, read the store once, build the pane's quota view at most once
/// a replay (a pass between replays is handed the one it built), its line D unit's words standing
/// while the clock alone moves, and write nothing; the card's snapshot is posted and carries the
/// replay. Negative control: a memo replayed on every read replays 600 times.
#[test]
fn the_outlook_replays_at_most_once_a_minute() {
    let dir = seeded();
    let before = files(dir.path());
    let roster = roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let start = Instant::now();
    let _ = crate::work_counts::take_account_store_reads();
    let own = Some(Figure::Exact(580));
    let mut views: Vec<QuotaView> = Vec::new();
    for s in 0..600i64 {
        let instant = start + Duration::from_secs(u64::try_from(s).expect("seconds"));
        let views_now = memo.quota((instant, NOW + s), Some(&roster), "me", own, true);
        let view = views_now
            .unwrap_or_else(|| panic!("second {s}: the pane's row has a view"))
            .view;
        if views.last() != Some(&view) {
            views.push(view);
        }
    }
    assert!(
        (1..=11).contains(&memo.replays),
        "at most once a minute: {} replays",
        memo.replays
    );
    assert!(
        (1..=memo.replays).contains(&views.len()),
        "at most one view a replay: {} views over {} replays",
        views.len(),
        memo.replays
    );
    let (first, last) = (&views[0], &views[views.len() - 1]);
    assert_eq!(
        first.fallback(),
        last.fallback(),
        "the unit's words stand while only the clock moves"
    );
    assert_eq!(
        crate::work_counts::take_account_store_reads(),
        1,
        "the store is read once while its stamps stand"
    );
    let snap = snapshot().expect("a snapshot is posted");
    assert!(snap.outlook.is_some(), "the snapshot carries the replay");
    assert!(
        memo.held
            .as_ref()
            .is_some_and(|h| h.read.is_ok() && h.roster == roster)
    );
    assert_eq!(before, files(dir.path()), "no file changed");

    // CONTROL: the store moves (a new reading saved): the next look re-reads it, and the
    // next replay is still a minute after the last.
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(NOW).expect("load");
    state.active = Some("alt1".into());
    store.save(&state).expect("save");
    let replays = memo.replays;
    let later = start + Duration::from_secs(600 + 11);
    let _ = memo.quota((later, NOW + 611), Some(&roster), "me", own, true);
    assert_eq!(
        crate::work_counts::take_account_store_reads(),
        1,
        "CONTROL: a moved store is read again"
    );
    assert!(memo.replays <= replays + 1);
    assert_eq!(
        memo.held
            .as_ref()
            .and_then(|h| h.read.as_ref().ok().map(|(st, _)| st.active.clone()))
            .flatten()
            .as_deref(),
        Some("alt1"),
        "CONTROL: the memo holds the store as it now is"
    );
    set_dir(None);
}

#[test]
fn a_failed_store_read_withdraws_the_forecast_without_spending_a_replay() {
    let dir = seeded();
    let roster = roster();
    let store = accounts::Store::at(dir.path());
    let source = std::fs::read(store.state_file()).unwrap();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..Default::default()
    };
    let start = Instant::now();
    assert!(memo.at(start, NOW, Some(&roster), false).is_some());
    let original = memo.replayed;
    let rejected_modified = std::fs::metadata(store.state_file())
        .unwrap()
        .modified()
        .unwrap()
        + Duration::from_secs(1);
    std::fs::write(store.state_file(), vec![b'!'; source.len()]).unwrap();
    let set_modified = || {
        std::fs::File::options()
            .write(true)
            .open(store.state_file())
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(rejected_modified))
            .unwrap();
    };
    set_modified();
    let rejected_stamp = stamp(&store.state_file());
    for (seconds, wall) in [(10, NOW + 10), (70, NOW + 70)] {
        assert!(
            memo.at(
                start + Duration::from_secs(seconds),
                wall,
                Some(&roster),
                true
            )
            .is_none()
        );
        assert!(memo.held.as_ref().unwrap().read.is_err());
        assert!(snapshot().unwrap().outlook.is_none());
        assert_eq!(memo.replays, 1);
        assert_eq!(memo.replayed, original, "a refusal is not a replay");
    }
    std::fs::write(store.state_file(), source).unwrap();
    set_modified();
    assert_eq!(stamp(&store.state_file()), rejected_stamp);
    assert!(
        memo.at(
            start + Duration::from_secs(71),
            NOW + 71,
            Some(&roster),
            false
        )
        .is_none(),
        "a failed read still observes the bounded look interval"
    );
    assert!(
        memo.at(
            start + Duration::from_secs(80),
            NOW + 80,
            Some(&roster),
            false
        )
        .is_some(),
        "a failed read retries even when the repaired file has the same stamp"
    );
    assert_eq!(
        memo.replays, 2,
        "recovery uses the last actual replay's clock"
    );
    assert!(snapshot().unwrap().outlook.is_some());
    set_dir(None);
}

#[test]
fn losing_or_changing_the_roster_withdraws_without_resetting_the_replay_clock() {
    let roster = roster();
    for (input, store_present) in [
        (None, true),
        (Some(Roster::default()), true),
        (
            Some(Roster {
                rows: vec![row("replacement", Class::Preferred)],
            }),
            true,
        ),
        (Some(roster.clone()), false),
    ] {
        let dir = seeded();
        let mut memo = OutlookMemo {
            reads: Some(scratch_reads()),
            ..Default::default()
        };
        let start = Instant::now();
        assert!(memo.at(start, NOW, Some(&roster), false).is_some());
        let original = memo.replayed;
        if !store_present {
            set_dir(None);
        }
        assert!(
            memo.at(
                start + Duration::from_secs(10),
                NOW + 10,
                input.as_ref(),
                true
            )
            .is_none(),
            "input {input:?}, store present {store_present}"
        );
        assert!(memo.outlook.is_none(), "the payload itself is withdrawn");
        assert!(snapshot().is_none_or(|s| s.outlook.is_none()));
        assert_eq!(memo.replays, 1);
        assert_eq!(memo.replayed, original);
        set_dir(Some(dir.path()));
        assert!(
            memo.at(
                start + Duration::from_secs(20),
                NOW + 20,
                Some(&roster),
                true
            )
            .is_none(),
            "restoring input cannot revive or recompute before the slot"
        );
        assert_eq!(memo.replays, 1);
        assert!(
            memo.at(
                start + Duration::from_secs(60),
                NOW + 60,
                Some(&roster),
                true
            )
            .is_some()
        );
        assert_eq!(memo.replays, 2);
        assert_eq!(memo.held.as_ref().unwrap().roster, roster);
        set_dir(None);
    }
}

#[test]
fn a_changed_policy_withdraws_forecasts_until_a_fresh_replay() {
    let changes: [fn() -> (Mode, bool); 2] = [|| (Mode::Off, false), || (Mode::Early, true)];
    for policy in changes {
        let dir = seeded();
        let roster = roster();
        let mut memo = OutlookMemo {
            reads: Some(scratch_reads()),
            ..Default::default()
        };
        let start = Instant::now();
        assert!(memo.at(start, NOW, Some(&roster), false).is_some());
        let original = memo.replayed;
        memo.reads = Some(Reads {
            policy,
            ..scratch_reads()
        });
        assert!(
            memo.at(
                start + Duration::from_secs(10),
                NOW + 10,
                Some(&roster),
                false
            )
            .is_none()
        );
        assert!(snapshot().unwrap().outlook.is_none());
        assert_eq!(memo.replays, 1);
        assert_eq!(memo.replayed, original);
        memo.reads = Some(scratch_reads());
        assert!(
            memo.at(
                start + Duration::from_secs(20),
                NOW + 20,
                Some(&roster),
                false
            )
            .is_none(),
            "restoring policy does not restore the old forecast"
        );
        assert_eq!(memo.replays, 1);
        assert!(
            memo.at(
                start + Duration::from_secs(60),
                NOW + 60,
                Some(&roster),
                false
            )
            .is_some()
        );
        assert_eq!(memo.replays, 2);
        set_dir(None);
        drop(dir);
    }
}

/// THE TIER THE GATED READ NAMED IS ITS ROW'S PLAN (§4.2): `Max 20x` read for `me` is one `plan`
/// row of its series, asked once however often the footer resolves; a new tier asks once more;
/// the next replay counts `me` at that tier. Nothing is asked before the store was read.
/// Negative control: the memo's check removed asks on every resolve.
#[test]
fn a_gated_tier_is_one_plan_row() {
    let dir = seeded();
    let roster = roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    memo.note_plan("me", "Max 20x", NOW);
    assert_eq!(
        memo.plans_noted, 0,
        "nothing is asked before the store is read"
    );
    let start = Instant::now();
    assert!(memo.at(start, NOW, Some(&roster), false).is_some());
    let (state, _) = accounts::Store::at(dir.path()).load(NOW).expect("loads");
    let plans = || -> Vec<account_series::PlanSaid> {
        account_series::read(dir.path(), "me", &state.machine)
            .into_iter()
            .filter_map(|row| match row.said {
                account_series::Said::Plan(plan) => Some(plan),
                _ => None,
            })
            .collect()
    };
    for s in 0..30 {
        memo.note_plan("me", "Max 20x", NOW + s);
    }
    assert_eq!(memo.plans_noted, 1, "one tier is asked once");
    let written = plans();
    assert_eq!(
        written
            .iter()
            .map(|p| (p.word.as_str(), p.tier.as_deref()))
            .collect::<Vec<_>>(),
        [("max", Some("20x"))]
    );
    memo.note_plan("me", "Max 5x", NOW + 31);
    assert_eq!(memo.plans_noted, 2, "a new tier is asked once more");
    assert_eq!(plans().len(), 2);
    let later = start + Duration::from_secs(61);
    let out = memo
        .at(later, NOW + 61, Some(&roster), true)
        .expect("replayed");
    let me = out.rows.iter().find(|r| r.label == "me").expect("me");
    assert_eq!(
        (me.plan.m, me.plan.title().as_deref()),
        (5, Some("Max 5x")),
        "the replay counts the tier the read named"
    );
    set_dir(None);
}

/// No harness state, no roster: no outlook and no snapshot, and nothing is read.
#[test]
fn no_store_no_outlook() {
    set_dir(None);
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let _ = crate::work_counts::take_account_store_reads();
    let own = Some(Figure::Exact(580));
    assert!(
        memo.quota((Instant::now(), NOW), Some(&roster()), "me", own, true)
            .is_none()
    );
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    assert!(snapshot().is_none());
    let dir = seeded();
    assert!(
        memo.quota((Instant::now(), NOW), None, "me", own, true)
            .is_none(),
        "no roster, no view"
    );
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    drop(dir);
    set_dir(None);
}

/// The pane's facts on `me`, with the quota view where the resolver gave one.
fn me_facts(quota: Option<QuotaView>) -> FooterFacts {
    FooterFacts {
        account: Some("me".into()),
        subscription: Some(SubscriptionDisplay {
            email: Some("me@example.test".into()),
            plan: Some("Max 20x".into()),
            weekly: Some("weekly 58% used".into()),
            label: Some("me".into()),
            ..SubscriptionDisplay::default()
        }),
        quota,
        // A stored login: the rotation can move the tab, so its card projects.
        route: Some(accounts::AuthRoute::Stored),
        ..facts()
    }
}

/// A WALLED ACCOUNT'S HOVER SAYS OUT ONCE: a store whose `me` read 97% of its week ten minutes
/// ago and then walled gives the footer's `weekly out`, and the quota view's title says `out now`
/// once, then what the policy does at the wall (the move to `alt1`), and never the 97% read
/// before it. Negative control: the footer's figure handed as a reading (97% used) is not said
/// over the standing wall either.
#[test]
fn a_walled_accounts_hover_says_out_once() {
    let dir = scratch();
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(NOW).expect("an empty store loads");
    let week = NOW + 3 * D;
    let at = NOW - 600;
    let _ = accounts::note_reading_held(&mut state, "me", &tap(970, week, at, false), at);
    let _ = accounts::note_wall(&mut state, "me", accounts::WEEK_LIMIT, Some(week), NOW, NOW);
    store.save(&state).expect("save");
    set_dir(Some(dir.path()));
    let roster = roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let outlook = memo
        .at(Instant::now(), NOW, Some(&roster), false)
        .expect("an outlook")
        .clone();
    for own in [Some(Figure::Out), Some(Figure::Exact(970))] {
        let mut facts = me_facts(Some(outlook.quota("me", own, true)));
        if let Some(sub) = facts.subscription.as_mut() {
            sub.weekly = Some(aterm_agent::harness::profile::WEEKLY_OUT.into());
        }
        let (title, _) = crate::claude_facts::fact_title(&facts, None, Fact::Account);
        assert!(
            title.starts_with("me \u{b7} ")
                && title.matches("out now").count() == 1
                && title.contains("the work moves to alt1 at the turn's end")
                && !title.contains("97%")
                && title.ends_with("click: accounts"),
            "{own:?}: {title}"
        );
    }
    drop(dir);
    set_dir(None);
}

/// The roster labels of the Accounts card's items, in order.
fn item_labels(rows: &[MenuRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| match row {
            MenuRow::Item { act, .. } => act
                .key()
                .strip_prefix("fact-menu/account/use/")
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[test]
fn unavailable_outlook_gallery() {
    use crate::claude_footer::probe;
    use aterm_agent::harness::outlook::{Refusal, Verdict};

    let (gallery, _permit) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_OUTLOOK_ACCESS_GALLERY");
    let dir = seeded_stop();
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(NOW).unwrap();
    for label in ["me", "alt1"] {
        accounts::note_auth(&mut state, label, accounts::Auth::SignedOut, NOW);
    }
    state.proven = false;
    store.save(&state).unwrap();
    let roster = roster();
    let mut manifest = String::from(
        "# Actual App account-card captures from synthetic local state; no provider or credentials.\nscene\tcolumns\trows\tfile\n",
    );
    for (name, mode, why) in [
        ("unavailable", Mode::Early, Refusal::AccountUnavailable),
        ("rotation-off", Mode::Off, Refusal::RotationOff),
    ] {
        let mut memo = OutlookMemo {
            reads: Some(Reads {
                policy: if mode == Mode::Off {
                    || (Mode::Off, false)
                } else {
                    || (Mode::Early, false)
                },
                ..scratch_reads()
            }),
            ..Default::default()
        };
        let outlook = memo.at(Instant::now(), NOW, Some(&roster), false).unwrap();
        assert_eq!(outlook.verdict, Verdict::Refused(why));
        assert!(outlook.week.is_none() && outlook.working.is_none());
        let (read, enrolled) = memo.held.as_ref().unwrap().read.clone().unwrap();
        let worker_rows = crate::fact_menu::account_menu_rows_from(
            &roster,
            &read,
            &enrolled,
            mode != Mode::Off,
            Some("me"),
            Some("me@example.test"),
            crate::fact_menu::SessionWeek::default(),
            NOW,
        );
        let before_files = files(dir.path());
        for cols in [120, 60] {
            let (mut app, wid, session) = glass_with(&AT_REST, me_facts(None));
            assert!(
                app.proxy.is_none(),
                "the fixture starts no live account reader"
            );
            app.config.serious_mode = Some(true);
            app.config.cursor_trail = Some(false);
            app.config.cursor_trail_style = Some("off".into());
            app.recompute_sparkle();
            app.pool
                .get(session)
                .unwrap()
                .ctx
                .timeline
                .lock()
                .unwrap()
                .set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
            if app.windows[&wid].cols != cols {
                assert!(app.apply_term_resize(wid, probe::ROWS, cols));
            }
            let mut screen = probe::screen_at(&AT_REST, probe::ROWS, cols);
            screen[0] = format!("SYNTHETIC OUTLOOK · {name} · no live account");
            probe::feed(&app, session, &screen);
            probe::refill(&mut app, wid);
            let _ = probe::pass(&mut app, wid);
            app.hold_account_menu_reads_for_test();
            let anchor = app
                .fact_anchor(wid, session, Fact::Account)
                .expect("account anchor");
            app.open_fact_menu(wid, session, Fact::Account, anchor, true);
            assert!(app.complete_account_menu_read_for_test(Ok(worker_rows.clone())));
            app.on_claude_footer_changed(session);
            let menu = app.autopilot_menu.as_ref().unwrap();
            assert_eq!(item_labels(&menu.rows), ["me", "alt1"]);
            assert!(menu.rows.iter().any(|row| matches!(row,
                MenuRow::Note { text, .. } if text.contains(&why.words())
            )));
            let png = crate::scripted_redraw_fixture::headless(&mut app, |app| {
                let (reply, receive) = std::sync::mpsc::channel();
                app.render_image(crate::control::ImageReq {
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: crate::control_auth::ConfinedImage::in_memory(),
                    clean: false,
                    session: None,
                    want_bytes: true,
                    want_metadata: false,
                    frame_metadata: Arc::new(std::sync::OnceLock::new()),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply,
                });
                receive
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap()
                    .unwrap()
                    .value
                    .2
                    .unwrap()
            });
            let rect = app
                .autopilot_menu
                .as_ref()
                .expect("menu survives capture")
                .rect
                .expect("account card was painted");
            let drawn = app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .skip(rect.row)
                .take(rect.h)
                .map(|row| {
                    row.iter()
                        .skip(rect.col)
                        .take(rect.w)
                        .map(|cell| cell.ch)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            let drawn = drawn.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(drawn.contains(&why.words()), "{name} at {cols}: {drawn}");
            let file = format!("outlook-{name}-{cols}.png");
            std::fs::write(gallery.join(&file), png).unwrap();
            manifest.push_str(&format!("{name}\t{cols}\t{}\t{file}\n", probe::ROWS));
            app.close_autopilot_menu();
        }
        assert_eq!(files(dir.path()), before_files);
    }
    std::fs::write(gallery.join("manifest.tsv"), manifest).unwrap();
    set_dir(None);
}

#[test]
fn paused_outlook_gallery() {
    use crate::claude_footer::probe;
    use aterm_agent::harness::account_progress::{
        AccountProgress, ProgressPhase, SESSION_AUTOPILOT_OFF_FINDING,
    };
    use aterm_agent::harness::outlook::Refusal;

    let (gallery, _permit) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_PAUSED_OUTLOOK_GALLERY");
    let dir = seeded_stop();
    let roster = roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..Default::default()
    };
    let outlook = memo
        .at(Instant::now(), NOW, Some(&roster), false)
        .unwrap()
        .clone();
    assert!(outlook.week.is_some());
    let own = Some(Figure::Exact(580));
    let view = outlook.quota("me", own, true);
    assert!(view.tail.verdict.is_some() && !view.chart.runs.is_empty());
    let observed = outlook
        .observations_only(Refusal::SessionPaused)
        .quota("me", own, true);
    assert!(observed.tail.verdict.is_none() && observed.chart.runs.is_empty());
    let (read, enrolled) = memo.held.as_ref().unwrap().read.clone().unwrap();
    let worker_rows = crate::fact_menu::account_menu_rows_from(
        &roster,
        &read,
        &enrolled,
        true,
        Some("me"),
        Some("me@example.test"),
        crate::fact_menu::SessionWeek::default(),
        NOW,
    );
    let before_files = files(dir.path());
    let mut manifest = String::from(
        "# Synthetic two-account observations, actual headless App/Painter; no provider or credentials.\nscene\tcolumns\trows\tfile\n",
    );
    for paused in [false, true] {
        let name = if paused { "paused" } else { "permitted" };
        for cols in [120, 60] {
            let mut facts = me_facts(Some(view.clone()));
            facts.quota_observed = Some(observed.clone());
            if paused {
                let mut progress = AccountProgress::unknown(NOW - 600);
                progress.phase = ProgressPhase::Paused;
                progress.forecasts_paused = true;
                progress.account = Some("me".into());
                progress.known = Some(2);
                progress.details.push(SESSION_AUTOPILOT_OFF_FINDING.into());
                facts.account_progress = Some(progress);
            }
            let (mut app, wid, session) = glass_with(&AT_REST, facts);
            assert!(app.proxy.is_none(), "no live account reader");
            app.config.serious_mode = Some(true);
            app.config.cursor_trail = Some(false);
            app.config.cursor_trail_style = Some("off".into());
            app.recompute_sparkle();
            let ctx = &app.pool.get(session).unwrap().ctx;
            ctx.timeline
                .lock()
                .unwrap()
                .set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
            if paused {
                ctx.meta.lock().unwrap().set_autopilot(
                    Some(aterm_agent::supervise::Autopilot::Off.word().into()),
                    None,
                );
            }
            if app.windows[&wid].cols != cols {
                assert!(app.apply_term_resize(wid, probe::ROWS, cols));
            }
            let mut screen = probe::screen_at(&AT_REST, probe::ROWS, cols);
            screen[0] = format!("SYNTHETIC TWO-ACCOUNT OUTLOOK · {name}");
            probe::feed(&app, session, &screen);
            probe::refill(&mut app, wid);
            let _ = probe::pass(&mut app, wid);
            let facts = app
                .pool
                .get(session)
                .unwrap()
                .ctx
                .timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .clone();
            assert_eq!(facts.account_forecasts_paused(), paused);
            assert_eq!(facts.quota.as_ref(), Some(&view));
            assert_eq!(
                facts.quota_view(),
                Some(if paused { &observed } else { &view }),
                "a paused recovery shows the observations alone"
            );
            assert_eq!(snapshot().unwrap().outlook.as_deref(), Some(&outlook));
            app.hold_account_menu_reads_for_test();
            let anchor = app
                .fact_anchor(wid, session, Fact::Account)
                .expect("account anchor");
            app.open_fact_menu(wid, session, Fact::Account, anchor, true);
            assert!(app.complete_account_menu_read_for_test(Ok(worker_rows.clone())));
            app.on_claude_footer_changed(session);
            assert_eq!(
                item_labels(&app.autopilot_menu.as_ref().unwrap().rows),
                ["me", "alt1"]
            );
            assert_eq!(app.autopilot_menu.as_ref().unwrap().rows.iter().any(|row| {
                matches!(row, MenuRow::Note { text, .. } if text.contains(&Refusal::SessionPaused.words()))
            }), paused);
            let png = crate::scripted_redraw_fixture::headless(&mut app, |app| {
                let (reply, receive) = std::sync::mpsc::channel();
                app.render_image(crate::control::ImageReq {
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: crate::control_auth::ConfinedImage::in_memory(),
                    clean: false,
                    session: None,
                    want_bytes: true,
                    want_metadata: false,
                    frame_metadata: Arc::new(std::sync::OnceLock::new()),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply,
                });
                receive
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap()
                    .unwrap()
                    .value
                    .2
                    .unwrap()
            });
            let file = format!("outlook-{name}-{cols}.png");
            std::fs::write(gallery.join(&file), png).unwrap();
            let rect = app
                .autopilot_menu
                .as_ref()
                .expect("menu survives capture")
                .rect
                .expect("account card was painted");
            let drawn = app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .skip(rect.row)
                .take(rect.h)
                .map(|row| {
                    row.iter()
                        .skip(rect.col)
                        .take(rect.w)
                        .map(|cell| cell.ch)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            let drawn = drawn.split_whitespace().collect::<Vec<_>>().join(" ");
            if paused {
                assert!(
                    drawn.contains(&Refusal::SessionPaused.words()),
                    "{cols}: {drawn}"
                );
            }
            manifest.push_str(&format!("{name}\t{cols}\t{}\t{file}\n", probe::ROWS));
            app.close_autopilot_menu();
        }
    }
    assert_eq!(files(dir.path()), before_files);
    std::fs::write(gallery.join("manifest.tsv"), manifest).unwrap();
    set_dir(None);
}

#[test]
fn watching_account_observations_gallery() {
    use crate::claude_footer::{probe, store_figures};
    use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};
    use aterm_agent::harness::outlook::{Fresh, Window};
    use aterm_agent::harness::profile::WeeklyFigure;

    let (gallery, _permit) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_ACCOUNT_OBSERVATION_GALLERY");
    let now = i64::try_from(aterm_time::unix_secs().expect("fixture clock")).unwrap();
    let sampled_at = now - 20 * 60;
    let future_at = now + 120;
    let reset = now + 3 * D;
    let dir = scratch();
    let store = accounts::Store::at(dir.path());
    let (mut state, _) = store.load(now).unwrap();
    state.active = Some("me".into());
    for (label, week_pct) in [("me", 25), ("alt1", 50)] {
        let _ = accounts::note_sample(
            &mut state,
            label,
            accounts::Sample {
                week_pct: Some(week_pct),
                week_reset: Some(reset),
                at: sampled_at,
                ..Default::default()
            },
            now,
        );
    }
    // Model a retained clock-skewed record, not a local tap call (which clamps its clock).
    state.accounts.get_mut("me").unwrap().windows.insert(
        accounts::WEEK_LIMIT.into(),
        WindowFig {
            pct_x10: 750,
            reset,
            at: future_at,
            by: state.machine.clone(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        },
    );
    store.save(&state).unwrap();
    set_dir(Some(dir.path()));
    let roster = roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..Default::default()
    };
    let outlook = memo
        .at(Instant::now(), now, Some(&roster), false)
        .unwrap()
        .clone();
    let (read, enrolled) = memo.held.as_ref().unwrap().read.clone().unwrap();
    let account = read.accounts.get("me").unwrap();
    assert_eq!(account.windows[accounts::WEEK_LIMIT].at, future_at);
    let [tapped, sampled] = store_figures(account, now);
    assert!(
        tapped.is_none(),
        "the future record is not current evidence"
    );
    let admitted = WeeklyFigure::newer(tapped, sampled).expect("older admitted sample");
    assert_eq!((admitted.at, admitted.used_x10), (sampled_at, 250));
    let week = outlook
        .rows
        .iter()
        .find(|row| row.label == "me")
        .unwrap()
        .window(Window::Week)
        .unwrap();
    assert_eq!(week.at, Some(sampled_at));
    assert_eq!(week.used_x10, Some(250));
    assert_eq!(week.fresh, Fresh::AsOf);
    assert_eq!(week.source.unwrap().kind, "usage");
    let weekly = admitted.words(now).unwrap();
    let worker_rows = crate::fact_menu::account_menu_rows_from(
        &roster,
        &read,
        &enrolled,
        true,
        Some("me"),
        Some("me@example.test"),
        crate::fact_menu::SessionWeek {
            label: Some("me".into()),
            figure: Some(admitted),
            ..Default::default()
        },
        now,
    );
    let mut progress = AccountProgress::unknown(now);
    progress.phase = ProgressPhase::Watching;
    progress.account = Some("me".into());
    progress.known = Some(2);
    // No check is due, so the words read no clock.
    let rotation = aterm_agent::harness::footer::rotation_words(&progress, |_| String::new());
    let facts = FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some("me@example.test".into()),
            plan: Some("Max 20x".into()),
            weekly: Some(weekly.clone()),
            weekly_figure: Some(admitted),
            label: Some("me".into()),
            ..Default::default()
        }),
        account_progress: Some(progress),
        quota: Some(outlook.quota(
            "me",
            Some(aterm_agent::harness::outlook::Figure::of_weekly(
                &admitted, now,
            )),
            true,
        )),
        ..facts()
    };
    let before_files = files(dir.path());
    let mut manifest = String::from(
        "# Synthetic admitted account observations in the actual headless App/Painter; no provider or credentials.\nscene\tcolumns\trows\tfile\n",
    );
    let source = std::fs::read(store.state_file()).unwrap();
    for (scene, unavailable) in [("watching", false), ("unavailable", true)] {
        let mut facts = facts.clone();
        let result = if unavailable {
            std::fs::write(store.state_file(), b"not an account state").unwrap();
            let instant = Instant::now() + Duration::from_secs(10);
            assert!(memo.at(instant, now + 10, Some(&roster), true).is_none());
            let why = memo
                .held
                .as_ref()
                .unwrap()
                .read
                .as_ref()
                .unwrap_err()
                .clone();
            let own = Some(aterm_agent::harness::outlook::Figure::of_weekly(
                &admitted, now,
            ));
            assert!(
                memo.quota((instant, now + 10), Some(&roster), "me", own, true)
                    .is_none()
            );
            (facts.quota, facts.quota_observed) = (None, None);
            Err(why)
        } else {
            Ok(worker_rows.clone())
        };
        assert_eq!(snapshot().unwrap().outlook.is_none(), unavailable);
        for cols in [120, 60] {
            let (mut app, wid, session) = glass_with(&AT_REST, facts.clone());
            assert!(app.proxy.is_none(), "no live account reader");
            app.config.serious_mode = Some(true);
            app.config.cursor_trail = Some(false);
            app.config.cursor_trail_style = Some("off".into());
            app.recompute_sparkle();
            app.pool
                .get(session)
                .unwrap()
                .ctx
                .timeline
                .lock()
                .unwrap()
                .set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
            if app.windows[&wid].cols != cols {
                assert!(app.apply_term_resize(wid, probe::ROWS, cols));
            }
            let mut screen = probe::screen_at(&AT_REST, probe::ROWS, cols);
            screen[0] = "SYNTHETIC ACCOUNT OBSERVATIONS · older admitted sample".into();
            probe::feed(&app, session, &screen);
            probe::refill(&mut app, wid);
            let _ = probe::pass(&mut app, wid);
            let (shown, cwd) = app.shown_facts(session).unwrap();
            assert_eq!(
                shown.subscription.as_ref().unwrap().weekly_figure,
                Some(admitted)
            );
            assert_eq!(shown.quota.is_none(), unavailable);
            let observed = shown.account_progress_view().unwrap();
            assert_eq!((observed.known, observed.eligible), (Some(2), None));
            // The view's words where the outlook replayed; the profile's own without one.
            let (title_week, spoken_week) = if unavailable {
                (weekly.clone(), weekly.replace('\u{2265}', "at least "))
            } else {
                (
                    "at least 25% of its week used".to_owned(),
                    "at least 25 percent of its week used".to_owned(),
                )
            };
            for (said, expected_week) in [
                (app.fact_title(session, Fact::Account).0, title_week),
                (
                    crate::claude_facts::fact_spoken_of(
                        &shown,
                        cwd.as_deref(),
                        Fact::Account,
                        aterm_agent::harness::footer::FactMask::default(),
                    )
                    .0,
                    spoken_week,
                ),
            ] {
                // Said last, lower-cased on the title and capitalised aloud.
                let rotation_at = said
                    .find(&rotation[1..])
                    .unwrap_or_else(|| panic!("{cols}: the rotation's words: {said}"));
                let week_at = said
                    .find(&expected_week)
                    .unwrap_or_else(|| panic!("{cols}: {expected_week:?} in {said}"));
                assert!(week_at < rotation_at, "{cols}: the rotation last: {said}");
            }
            app.hold_account_menu_reads_for_test();
            let anchor = app
                .fact_anchor(wid, session, Fact::Account)
                .expect("actual account anchor");
            app.open_fact_menu(wid, session, Fact::Account, anchor, true);
            assert!(app.complete_account_menu_read_for_test(result.clone()));
            app.on_claude_footer_changed(session);
            let rows = &app.autopilot_menu.as_ref().unwrap().rows;
            if unavailable {
                assert!(item_labels(rows).is_empty());
                assert!(!rows.iter().any(|row| matches!(row, MenuRow::Plot { .. })));
            } else {
                assert_eq!(item_labels(rows), ["me", "alt1"]);
            }
            let png = crate::scripted_redraw_fixture::headless(&mut app, |app| {
                let (reply, receive) = std::sync::mpsc::channel();
                app.render_image(crate::control::ImageReq {
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: crate::control_auth::ConfinedImage::in_memory(),
                    clean: false,
                    session: None,
                    want_bytes: true,
                    want_metadata: false,
                    frame_metadata: Arc::new(std::sync::OnceLock::new()),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply,
                });
                receive
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap()
                    .unwrap()
                    .value
                    .2
                    .unwrap()
            });
            let file = format!("account-observations-{scene}-{cols}.png");
            std::fs::write(gallery.join(&file), png).unwrap();
            let rect = app
                .autopilot_menu
                .as_ref()
                .expect("menu survives capture")
                .rect
                .expect("account card painted");
            let drawn = app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .skip(rect.row)
                .take(rect.h)
                .map(|row| {
                    row.iter()
                        .skip(rect.col)
                        .take(rect.w)
                        .map(|cell| cell.ch)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ");
            // The admitted figure, used (never `% left`, on either card): the outlook's card says
            // its bound; the card without one says the pane's own weekly words.
            assert!(!drawn.contains("% left"), "{cols}: {drawn}");
            let figure = if unavailable {
                weekly.clone()
            } else {
                format!("\u{2265}{}%", admitted.used_x10 / 10)
            };
            assert!(drawn.contains(&figure), "{cols}: {figure:?} in {drawn}");
            if unavailable {
                assert!(
                    drawn.contains("Account state unavailable"),
                    "{cols}: {drawn}"
                );
            }
            manifest.push_str(&format!("{scene}\t{cols}\t{}\t{file}\n", probe::ROWS));
            app.close_autopilot_menu();
        }
    }
    std::fs::write(store.state_file(), source).unwrap();
    assert_eq!(
        files(dir.path()),
        before_files,
        "capture leaves the input history unchanged"
    );
    std::fs::write(gallery.join("manifest.tsv"), manifest).unwrap();
    set_dir(None);
}

/// The first box, block, braille or legacy-computing glyph in `s` (U+2500-259F, U+2800-28FF,
/// U+1FB00-1FBFF): a drawing a reader would hear as noise.
fn box_drawing(s: &str) -> Option<char> {
    s.chars().find(|c| {
        ('\u{2500}'..='\u{259f}').contains(c)
            || ('\u{2800}'..='\u{28ff}').contains(c)
            || ('\u{1fb00}'..='\u{1fbff}').contains(c)
    })
}

/// T21 and §10: OPENING THE ACCOUNTS CARD READS NO FILE ON THIS THREAD. The resolver (here, the
/// test's own stand-in) posts its snapshot, and the account reader answers off this thread (here,
/// the test answers for it with the rows of the same state); the card opens on both, its items
/// in the quota view's order (here the roster's) with the outlook's drawn rows, and neither the store nor any file the outlook's
/// memo looks at (the store's and the logins' stamps, a series' stamp or its read) is touched on
/// this thread while it opens, refreshes and closes. Negative control: a card that runs the
/// memo itself on open reads the store and stats its files.
#[test]
fn the_card_opens_on_the_snapshot_and_reads_nothing() {
    let dir = seeded_stop();
    let roster = pool_roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    assert!(memo.at(Instant::now(), NOW, Some(&roster), false).is_some());
    assert_eq!(
        crate::work_counts::take_account_store_reads(),
        1,
        "the resolver read the store"
    );
    assert!(
        crate::work_counts::take_outlook_file_looks() > 3,
        "CONTROL: the resolver's looks are counted"
    );
    // The account reader's rows of the same store, as its worker would read them.
    let (state, enrolled) = memo
        .held
        .as_ref()
        .and_then(|h| h.read.clone().ok())
        .expect("read");
    let (state, enrolled) = (&state, &enrolled);
    let worker_rows = crate::fact_menu::account_menu_rows_from(
        &roster,
        state,
        enrolled,
        true,
        Some("me"),
        Some("me@example.test"),
        crate::fact_menu::SessionWeek::default(),
        NOW,
    );
    let (mut app, wid, session) = glass_with(&AT_REST, me_facts(None));
    app.pool
        .get(session)
        .expect("the session")
        .ctx
        .timeline
        .lock()
        .expect("its timeline")
        .set_agent_reader_program_for_test(Some(aterm_phase::Program::Claude));
    app.hold_account_menu_reads_for_test();
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), false);
    assert!(
        app.complete_account_menu_read_for_test(Ok(worker_rows)),
        "the card asked the account reader"
    );
    app.on_claude_footer_changed(session);
    assert_eq!(
        (
            crate::work_counts::take_account_store_reads(),
            crate::work_counts::take_outlook_file_looks()
        ),
        (0, 0),
        "the card opened on the snapshot: no read and no look on this thread"
    );
    let menu = app.autopilot_menu.as_ref().expect("the card is open");
    assert_eq!(
        item_labels(&menu.rows),
        ["me", "alt1", "pool1"],
        "roster order"
    );
    assert!(
        menu.rows.iter().any(|r| matches!(r, MenuRow::Plot { .. })),
        "the outlook's work row and lanes are on the card"
    );
    app.close_autopilot_menu();
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    assert_eq!(crate::work_counts::take_outlook_file_looks(), 0);
    drop(dir);
    set_dir(None);
}

/// T22 and T23 on the card the window draws: at 60, 44 and 30 cells and with room for every row
/// or a short pane, every drawn row fits, every `Plot` is captioned, no spoken word and no
/// hover title holds a box, block, braille or legacy-computing glyph, and the items keep the
/// quota view's order (here the roster's) and choose their own account. Negative controls: an
/// uncaptioned plot; a lane's levels in its caption; items in the outlook's order reversed.
#[test]
fn the_cards_rows_fit_speak_and_keep_roster_order() {
    let dir = seeded_stop();
    let roster = pool_roster();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let outlook = Arc::new(
        memo.at(Instant::now(), NOW, Some(&roster), false)
            .expect("an outlook")
            .clone(),
    );
    let (state, enrolled) = memo
        .held
        .as_ref()
        .and_then(|h| h.read.clone().ok())
        .expect("read");
    let (state, enrolled) = (&state, &enrolled);
    let (accounts, _) = crate::fact_menu::account_menu_rows_from(
        &roster,
        state,
        enrolled,
        true,
        Some("me"),
        Some("me@example.test"),
        crate::fact_menu::SessionWeek::default(),
        NOW,
    );
    let mut plots = 0;
    for room in [60, 44, 30] {
        for height in [None, Some(12)] {
            let facts = crate::fact_menu::FactMenuFacts {
                title: "Accounts \u{b7} tab 1".into(),
                spoken: "Accounts, tab 1".into(),
                room,
                accounts: accounts.clone(),
                outlook: Some(Arc::clone(&outlook)),
                account_row: Some("me".into()),
                movable: true,
                height,
                cli: Some(aterm_agent::harness::accounts::Cli::Claude),
                ..crate::fact_menu::FactMenuFacts::default()
            };
            let rows = crate::fact_menu::rows(Fact::Account, &facts);
            let what = format!("{room} cells, height {height:?}");
            assert_eq!(item_labels(&rows), ["me", "alt1", "pool1"], "{what}");
            for row in &rows {
                match row {
                    MenuRow::Plot { cells, spoken, .. } => {
                        assert!(cells.len() <= room, "{what}: a plot of {}", cells.len());
                        assert!(!spoken.trim().is_empty(), "{what}: an uncaptioned plot");
                        assert_eq!(box_drawing(spoken), None, "{what}: {spoken:?}");
                        plots += 1;
                    }
                    MenuRow::Note { text, spoken, .. } => {
                        let w = crate::tab_menu::display_cells(text);
                        assert!(w <= room, "{what}: {text:?} is {w} cells");
                        assert_eq!(box_drawing(spoken), None, "{what}: {spoken:?}");
                    }
                    MenuRow::Item { spoken, .. } => {
                        assert_eq!(box_drawing(spoken), None, "{what}: {spoken:?}");
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(plots > 0, "the card draws its work row and lanes");
    let facts = me_facts(Some(outlook.quota("me", Some(Figure::Exact(580)), true)));
    let (title, _) = crate::claude_facts::fact_title(&facts, None, Fact::Account);
    let (spoken, _) = crate::claude_facts::fact_spoken_of(
        &facts,
        None,
        Fact::Account,
        aterm_agent::harness::footer::FactMask::default(),
    );
    for words in [&title, &spoken] {
        assert_eq!(box_drawing(words), None, "{words:?}");
    }
    assert!(title.starts_with("me \u{b7} "), "{title:?}");
    drop(dir);
    set_dir(None);
}

/// The card's rows for the store's `accounts` at `height`, on `me`'s pane, `movable` or not.
fn card_menu(
    outlook: &Arc<aterm_agent::harness::outlook::Outlook>,
    accounts: &[crate::fact_menu::AccountMenuRow],
    height: Option<usize>,
    movable: bool,
) -> Vec<MenuRow> {
    let facts = crate::fact_menu::FactMenuFacts {
        title: "Accounts \u{b7} tab 1".into(),
        spoken: "Accounts, tab 1".into(),
        room: 60,
        accounts: accounts.to_vec(),
        outlook: Some(Arc::clone(outlook)),
        account_row: Some("me".into()),
        movable,
        height,
        cli: Some(aterm_agent::harness::accounts::Cli::Claude),
        ..crate::fact_menu::FactMenuFacts::default()
    };
    crate::fact_menu::rows(Fact::Account, &facts)
}

/// The seeded outlook over `roster` and the store's rows for it.
fn seeded_card(
    roster: &Roster,
) -> (
    aterm_tempfile::TempDir,
    Arc<aterm_agent::harness::outlook::Outlook>,
    Vec<crate::fact_menu::AccountMenuRow>,
) {
    let dir = seeded_stop();
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let outlook = Arc::new(
        memo.at(Instant::now(), NOW, Some(roster), false)
            .expect("an outlook")
            .clone(),
    );
    let (state, enrolled) = memo
        .held
        .as_ref()
        .and_then(|h| h.read.clone().ok())
        .expect("read");
    let (accounts, _) = crate::fact_menu::account_menu_rows_from(
        roster,
        &state,
        &enrolled,
        true,
        Some("me"),
        Some("me@example.test"),
        crate::fact_menu::SessionWeek::default(),
        NOW,
    );
    (dir, outlook, accounts)
}

/// A lane row's account: its caption names its week (`alt1's week, …`).
fn lane_label(row: &MenuRow) -> Option<&str> {
    match row {
        MenuRow::Plot { spoken, .. } => spoken.split_once("'s week, ").map(|(label, _)| label),
        _ => None,
    }
}

fn is_legend(row: &MenuRow) -> bool {
    matches!(row, MenuRow::Plot { spoken, .. } if spoken.starts_with("Legend:"))
}

/// AN ITEM AND ITS LANE ARE ONE UNIT, AND ONLY THE LEGEND CLOSES THE CARD'S DRAWN ROWS (§1.4):
/// each lane follows its own account's item; a card account the store does not list draws
/// neither its item nor its lane; the pin item follows the legend directly and a rule under
/// anything else, a lane included, at every height. Negative controls: a mapping that skips an
/// unlisted item but keeps its lane leaves `alt1`'s lane under `me`'s; a rule skipped whenever the
/// card ends on any drawn row puts the pin item straight under `alt1`'s lane once the legend
/// gives way.
#[test]
fn an_item_and_its_lane_are_one_unit_and_the_legend_closes_the_card() {
    let (dir, outlook, accounts) = seeded_card(&roster());
    let mut ended_on_a_lane = false;
    let heights = std::iter::once(None).chain((0..=40).map(Some));
    for height in heights {
        let rows = card_menu(&outlook, &accounts, height, true);
        for (i, row) in rows.iter().enumerate() {
            if let Some(label) = lane_label(row) {
                assert_eq!(
                    item_labels(&rows[i - 1..i]),
                    [label],
                    "h{height:?}: {label}'s lane under its own item"
                );
            }
        }
        let pin = rows
            .iter()
            .position(|r| {
                matches!(r, MenuRow::Item { glyph, label, .. }
                if *glyph == crate::fact_menu::PIN_GLYPH && label.starts_with("No pinned"))
            })
            .expect("the pin item");
        let before = &rows[pin - 1];
        assert!(
            is_legend(before) || matches!(before, MenuRow::Rule),
            "h{height:?}: the pin item under {before:?}"
        );
        if pin >= 2 && matches!(before, MenuRow::Rule) && lane_label(&rows[pin - 2]).is_some() {
            ended_on_a_lane = true;
        }
    }
    assert!(
        ended_on_a_lane,
        "some height drops the legend and keeps the lanes"
    );
    drop(dir);
    set_dir(None);

    let (dir, outlook, accounts) = seeded_card(&pool_roster());
    let listed: Vec<_> = accounts
        .iter()
        .filter(|a| a.label.split_whitespace().next() != Some("alt1"))
        .cloned()
        .collect();
    let rows = card_menu(&outlook, &listed, None, true);
    assert_eq!(item_labels(&rows), ["me", "pool1"]);
    assert!(
        rows.iter().all(|r| lane_label(r) != Some("alt1")),
        "alt1's lane went with its item"
    );
    assert!(
        rows.iter().any(|r| lane_label(r) == Some("me")),
        "CONTROL: me's lane stands"
    );
    drop(dir);
    set_dir(None);
}

/// A TAB THE ROTATION CANNOT MOVE IS PROJECTED NOWHERE ON THE CARD THE WINDOW DRAWS (§2.2): with
/// the footer's route off the account ([`crate::fact_menu::FactMenuFacts::movable`] false) no
/// drawn row outside the legend holds the projected line's ink and no note says `╳`.
/// Negative control: the same card for a movable tab projects the seeded stop.
#[test]
fn a_card_for_a_tab_the_rotation_cannot_move_projects_nothing() {
    let (dir, outlook, accounts) = seeded_card(&pool_roster());
    let projects = |rows: &[MenuRow]| {
        rows.iter().any(|r| match r {
            MenuRow::Plot { cells, .. } if !is_legend(r) => cells
                .iter()
                .any(|c| c.ink == crate::autopilot_menu::PlotInk::Ahead),
            _ => false,
        })
    };
    let verdict_warns = |rows: &[MenuRow]| {
        rows.iter()
            .any(|r| matches!(r, MenuRow::Note { text, .. } if text.contains('\u{2573}')))
    };
    let moved = card_menu(&outlook, &accounts, None, true);
    assert!(
        projects(&moved) && verdict_warns(&moved),
        "CONTROL: {moved:?}"
    );
    let still = card_menu(&outlook, &accounts, None, false);
    assert!(!projects(&still), "{still:?}");
    assert!(!verdict_warns(&still), "{still:?}");
    assert!(
        still
            .iter()
            .any(|r| matches!(r, MenuRow::Note { text, .. } if text.contains("not projected"))),
        "{still:?}"
    );
    drop(dir);
    set_dir(None);
}

/// What a person would notice the window raise: a message on screen or in the log, a digest
/// entry (`NEEDS YOU` among them), a bell, a flash of the border, a wake armed for motion.
#[derive(Debug, PartialEq, Eq)]
struct Raised {
    live: usize,
    logged: usize,
    digest: u64,
    bells: u64,
    flash: bool,
    wakes: usize,
}

impl Raised {
    fn of(app: &App, wid: WindowId, session: u64) -> Self {
        Self {
            live: app.messages.live_rows().count(),
            logged: app.messages.log().records().count(),
            digest: app.messages.digest_revision(),
            bells: app
                .pool
                .get(session)
                .map_or(0, |entry| entry.ctx.bells.read().total),
            flash: app.windows[&wid].bell_flash.deadline().is_some(),
            wakes: crate::work_counts::take_footer_wakes_armed(),
        }
    }
}

/// ACCEPTANCE 5: A FORECAST INTERRUPTS NOBODY. A replay that projects a stop reaches the pane's
/// rule in its warn words, the Accounts card opens on it, a later replay refreshes the open
/// card, and it closes: no message is shown or logged, no digest entry or `NEEDS YOU` is raised,
/// no bell rings, the border does not flash and no wake is armed for motion. Control: the same
/// observation sees a posted account entry.
#[test]
fn a_forecast_interrupts_nobody() {
    let dir = seeded_stop();
    let roster = roster();
    let (mut app, wid, session) = glass_with(&AT_REST, me_facts(None));
    for _ in 0..3 {
        pass(&mut app, wid);
    }
    let before = Raised::of(&app, wid, session);
    let mut memo = OutlookMemo {
        reads: Some(scratch_reads()),
        ..OutlookMemo::default()
    };
    let view = memo
        .quota(
            (Instant::now(), NOW),
            Some(&roster),
            "me",
            Some(Figure::Exact(580)),
            true,
        )
        .expect("a view")
        .view;
    let verdict = view.tail.verdict.clone().expect("a verdict");
    assert!(
        verdict.ink == aterm_agent::harness::quota_view::Role::Warn,
        "the seed projects a stop: {verdict:?}"
    );
    publish(&app, session, me_facts(Some(view)));
    for _ in 0..5 {
        pass(&mut app, wid);
    }
    app.open_fact_menu(wid, session, Fact::Account, (0, 0), false);
    let later = Instant::now() + Duration::from_secs(61);
    let _ = memo.at(later, NOW + 61, Some(&roster), true);
    app.on_claude_footer_changed(session);
    for _ in 0..5 {
        pass(&mut app, wid);
    }
    app.close_autopilot_menu();
    pass(&mut app, wid);
    assert_eq!(Raised::of(&app, wid, session), before, "nothing was raised");

    // CONTROL: an account entry posted is seen.
    let entry = crate::message_reporters::accounts_signed_out_entry(&["alt1".to_owned()])
        .expect("an entry");
    let _ = app.messages.post(
        entry.message().expect("an entry that passes"),
        crate::messages_host::wall_stamp_now(),
        Instant::now(),
    );
    assert_ne!(
        Raised::of(&app, wid, session),
        before,
        "CONTROL: a posted entry is raised"
    );
    drop(dir);
    set_dir(None);
}
