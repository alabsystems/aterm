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
use aterm_agent::harness::profile::SubscriptionDisplay;

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
        rows: vec![row("me", Class::Dedicated), row("alt1", Class::Dedicated)],
    }
}

/// `me`, `alt1` and a never-read `pool1`.
fn pool_roster() -> Roster {
    Roster {
        rows: vec![
            row("me", Class::Dedicated),
            row("alt1", Class::Dedicated),
            row("pool1", Class::Shared),
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
/// replay it at most eleven times, read the store once, give the pane the same glance but where
/// the clock alone moves it (`me`'s 5-hour figure, read ten minutes before, stops being a figure
/// of now at its fifteenth minute and leaves the rule), and write nothing; the card's snapshot is
/// posted and carries the replay. Negative control: a memo replayed on every read replays 600
/// times.
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
    let mut glances = Vec::new();
    for s in 0..600i64 {
        let instant = start + Duration::from_secs(u64::try_from(s).expect("seconds"));
        let glance = memo.glance((instant, NOW + s), Some(&roster), "me", Some(580), true);
        let glance = glance.unwrap_or_else(|| panic!("second {s}: the pane's row has a glance"));
        if glances.last() != Some(&glance) {
            glances.push(glance);
        }
    }
    assert!(
        (1..=11).contains(&memo.replays),
        "at most once a minute: {} replays",
        memo.replays
    );
    assert_eq!(
        glances.len(),
        2,
        "a replay with nothing moved gives the same glance, but for the 5-hour figure's age: \
         {glances:#?}"
    );
    let (fresh, aged) = (&glances[0], &glances[1]);
    assert!(fresh.five.is_some() && aged.five.is_none(), "{glances:#?}");
    assert_eq!(
        (&aged.bar, &aged.reset, &aged.verdict, &aged.title),
        (&fresh.bar, &fresh.reset, &fresh.verdict, &fresh.title),
        "nothing else moved"
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
    let _ = memo.glance((later, NOW + 611), Some(&roster), "me", Some(580), true);
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
    assert!(
        memo.glance(
            (Instant::now(), NOW),
            Some(&roster()),
            "me",
            Some(580),
            true
        )
        .is_none()
    );
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    assert!(snapshot().is_none());
    let dir = seeded();
    assert!(
        memo.glance((Instant::now(), NOW), None, "me", Some(580), true)
            .is_none(),
        "no roster, no glance"
    );
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    drop(dir);
    set_dir(None);
}

/// The pane's facts on `me`, with `glance` where the resolver gave one.
fn me_facts(glance: Option<aterm_agent::harness::outlook::Glance>) -> FooterFacts {
    FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some("me@example.test".into()),
            plan: Some("Max 20x".into()),
            weekly: Some("weekly 42% left".into()),
            label: Some("me".into()),
            ..SubscriptionDisplay::default()
        }),
        glance,
        ..facts()
    }
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

fn box_drawing(s: &str) -> Option<char> {
    s.chars().find(|c| ('\u{2500}'..='\u{257f}').contains(c))
}

/// T21 and §10: OPENING THE ACCOUNTS CARD READS NO FILE ON THIS THREAD. The resolver (here, the
/// test's own stand-in) posts its snapshot, and the account reader answers off this thread (here,
/// the test answers for it with the rows of the same state); the card opens on both, its items
/// in roster order with the outlook's bars, and neither the store nor any file the outlook's
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
        "the outlook's bars are on the card"
    );
    app.close_autopilot_menu();
    assert_eq!(crate::work_counts::take_account_store_reads(), 0);
    assert_eq!(crate::work_counts::take_outlook_file_looks(), 0);
    drop(dir);
    set_dir(None);
}

/// T22 and T23 on the card the window draws: at 60, 44 and 30 cells and with room for every row
/// or a short pane, every drawn row fits, every `Plot` is captioned, no spoken word and no
/// hover title is a box-drawing glyph, and the roster's items keep roster order and choose their
/// own account. Negative controls: an uncaptioned plot; items in the outlook's order reversed.
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
                height,
                cli: Some(aterm_agent::harness::accounts::Cli::Claude),
                ..crate::fact_menu::FactMenuFacts::default()
            };
            let rows = crate::fact_menu::rows(Fact::Account, &facts);
            let what = format!("{room} cells, height {height:?}");
            assert_eq!(item_labels(&rows), ["me", "alt1", "pool1"], "{what}");
            for row in &rows {
                match row {
                    MenuRow::Plot { cells, spoken } => {
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
    assert!(plots > 0, "the card draws bars");
    let glance = outlook.glance("me", Some(580), true).expect("me's glance");
    let facts = me_facts(Some(glance));
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
    assert!(title.contains("me@example.test"), "{title:?}");
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
    let glance = memo
        .glance((Instant::now(), NOW), Some(&roster), "me", Some(580), true)
        .expect("a glance");
    let verdict = glance.verdict.clone().expect("a verdict");
    assert!(verdict.warn, "the seed projects a stop: {verdict:?}");
    publish(&app, session, me_facts(Some(glance)));
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
