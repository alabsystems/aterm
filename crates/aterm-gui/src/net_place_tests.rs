// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SINCE_MS: u64 = 1_800_000_000_000;

fn utc(_: i64) -> Option<i64> {
    Some(0)
}

fn words_at(now_ms: u64) -> Words<'static> {
    Words { now_ms, zone: &utc }
}

/// Requests made while an older observation is captured are serviced by the SAME owner.
/// Reconciliation consumes its accepted snapshot, without another cache read.
#[cfg(unix)]
#[test]
fn an_inflight_cache_read_cannot_overwrite_the_followers_newer_observation() {
    use aterm_netplace::place::{Known, Stay, StayAt};
    use std::sync::mpsc;
    use std::time::Duration;
    let hang = Duration::from_secs(60);
    let root = aterm_tempfile::tempdir().unwrap();
    let now = aterm_netplace::unix_ms_now();
    let start = now - 2 * aterm_netplace::words::AWAY_ENTRY_AFTER_MS;
    let home = aterm_netplace::sample::home(start - 60_000, 3);
    let away = aterm_netplace::sample::hotspot(start, start);
    let stay = |place: &Place, start_ms, end_ms, at| Stay {
        start_ms,
        end_ms,
        at,
        network: place.network.clone(),
        subnet: None,
        gateway: place.route.as_ref().and_then(|route| route.gateway),
        from_start: false,
    };
    aterm_netplace::record::write(
        &aterm_netplace::record::dir(root.path()),
        &aterm_netplace::record::Written {
            record: aterm_netplace::Record {
                stays: vec![
                    stay(&home, start - 60_000, Some(start), StayAt::AlabLan),
                    stay(&away, start, None, StayAt::Remote),
                ],
                remembered: vec![Known {
                    network: home.network.clone().unwrap(),
                    learned_ms: start - 60_000,
                    confirmed_ms: start,
                }],
            },
            ..Default::default()
        },
    )
    .unwrap();
    let desk = Arc::new(ObservationDesk::new());
    let (notice, notices) = mpsc::channel();
    assert!(desk.request().1);
    let (captured, capture) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (new_captured, new_capture) = mpsc::channel();
    let (new_release, new_released) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let worker_desk = Arc::clone(&desk);
    let path = root.path().to_owned();
    let mut reads = 0;
    let worker = std::thread::spawn(move || {
        let mut gates_ok = true;
        worker_desk.run(|| {
            reads += 1;
            let place = aterm_netplace::keeper::read_with(
                &path,
                aterm_netplace::Facts {
                    route: if reads == 1 {
                        home.route.clone()
                    } else {
                        away.route.clone()
                    },
                    ..Default::default()
                },
                now,
            );
            if reads == 1 {
                gates_ok &= captured.send(()).is_ok();
                gates_ok &= released.recv_timeout(hang).is_ok();
            } else if reads == 2 {
                gates_ok &= new_captured.send(()).is_ok();
                gates_ok &= new_released.recv_timeout(hang).is_ok();
            }
            Arc::new(place)
        });
        let _ = done.send((reads, gates_ok));
    });
    let captured = capture.recv_timeout(hang);
    let (_, duplicate_owner) = desk.request();
    let released = release.send(());
    let new_captured = new_capture.recv_timeout(hang);
    // The owner has accepted the first read without a window. A closed proxy must not
    // kill cache service or consume that pending accepted change.
    let (closed_notice, closed_notices) = mpsc::channel();
    drop(closed_notices);
    desk.register(Notice::Test(closed_notice));
    let alive_without_proxy = desk.state.lock_or_recover().running;
    desk.register(Notice::Test(notice));
    let first = notices.recv_timeout(hang);
    let new_released = new_release.send(());
    let second = notices.recv_timeout(hang);
    let snapshot = desk.state.lock_or_recover().place.clone();
    let mut app = crate::App::headless_for_test();
    app.follow_network_place_on(&desk, &words_at(now));
    settle(&mut app);
    let entries = away_entries_in(&app);
    let consumed = desk.take_change();
    desk.close();
    let completion = finished
        .recv_timeout(hang)
        .expect("the closed owner finishes");
    let joined = worker.join();
    assert!(joined.is_ok());
    let (read_count, gates_ok) = completion;
    assert!(gates_ok && released.is_ok() && new_released.is_ok());
    assert!(captured.is_ok());
    assert!(new_captured.is_ok());
    assert!(alive_without_proxy);
    assert!(!duplicate_owner);
    assert!(first.is_ok() && second.is_ok());
    assert!(
        read_count >= 2,
        "the request during I/O is serviced before proxy registration"
    );
    assert!(matches!(
        snapshot.unwrap().at,
        aterm_netplace::Where::Remote(_)
    ));
    assert_eq!(
        entries, 1,
        "the actual reconciliation states the newer away observation"
    );
    assert!(
        consumed.is_none(),
        "snapshot and change are consumed together"
    );
}

#[cfg(unix)]
#[test]
fn an_unknown_observation_clears_a_restored_away_entry_without_starting_io() {
    let mut app = crate::App::headless_for_test();
    let words = words_at(SINCE_MS + 10 * 60_000);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    app.state_network_place(Some(&away), &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 1);
    let desk = ObservationDesk::new();
    app.follow_network_place_on(&desk, &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0);
    assert!(desk.take_change().is_none());
    let held = desk.state.lock_or_recover();
    assert!(!held.running && !held.requested && held.read_at.is_none());
}

#[test]
#[cfg(unix)]
fn cache_spawn_failure_keeps_observation_age_and_pending_request() {
    let desk = ObservationDesk::new();
    assert!(desk.request().1);
    desk.failed_start();
    let held = desk.state.lock_or_recover();
    assert!(held.place.is_none() && held.read_at.is_none());
    assert!(held.requested && !held.running);
    drop(held);
    assert!(
        !desk.request().1,
        "failed spawn has a separate bounded retry"
    );
    // Retiring the failed attempt's backoff permits one owner, never two.
    desk.state.lock_or_recover().retry_at = Some(Instant::now());
    assert!(desk.request().1);
    assert!(!desk.request().1);
    desk.failed_start();
}

#[test]
fn cached_place_snapshots_do_not_copy_route_payloads() {
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let (_, control) = crate::work_counts::allocations_in(|| away.clone());
    assert!(control > 0, "the fixture must own heap payloads");
    let _held = hold_scoped(Some(away));
    let before = reads_asked();
    let (_, allocations) = crate::work_counts::allocations_in(|| {
        for _ in 0..64 {
            std::hint::black_box(cached());
        }
    });
    eprintln!("cached snapshot allocations: {allocations}; one deep clone: {control}");
    assert_eq!(reads_asked(), before, "cached snapshots start no read");
    assert_eq!(allocations, 0, "snapshot acquisition shares immutable data");
}

#[test]
fn captured_place_words_allocate_only_the_words_themselves() {
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let (_, words) = crate::work_counts::allocations_in(|| {
        for _ in 0..64 {
            std::hint::black_box(crate::network_deck::place_caption(Some(&away)));
        }
    });
    let expected = crate::network_deck::place_caption(Some(&away));
    let _held = hold_scoped(Some(away));
    let (caption, captured) = crate::work_counts::allocations_in(|| {
        for _ in 0..64 {
            std::hint::black_box(crate::network_deck::capture_place_caption());
        }
        crate::network_deck::capture_place_caption()
    });
    let (_, one) = crate::work_counts::allocations_in(crate::network_deck::capture_place_caption);
    assert_eq!(caption, expected);
    eprintln!(
        "64 direct captions: {words}; 65 cached captions: {captured}; one cached caption: {one}"
    );
    assert_eq!(captured - one, words, "cache access adds no heap copies");
}

#[test]
fn cached_place_snapshot_survives_replacement_and_releases_after_its_readers() {
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let home = aterm_netplace::sample::home(SINCE_MS + 60_000, 3);
    let _held = hold_scoped(Some(away.clone()));
    let old = cached().expect("published away snapshot");
    let second = latest().expect("same published snapshot");
    assert!(Arc::ptr_eq(&old, &second));
    let weak = Arc::downgrade(&old);
    hold(Some(home.clone()));
    let current = cached().expect("replacement snapshot");
    assert!(!Arc::ptr_eq(&old, &current));
    assert_eq!(*old, away, "a reader retains its captured observation");
    assert_eq!(*current, home);
    hold(None);
    assert_eq!(
        *current, home,
        "clearing the desk cannot invalidate a reader"
    );
    drop(old);
    assert!(weak.upgrade().is_some(), "the second reader still owns it");
    drop(second);
    assert!(
        weak.upgrade().is_none(),
        "retired snapshots are not retained"
    );
}

/// `sys`'s place is the place's own line and object, its unknown values `not_sampled` as every
/// other `sys` value that was not read, nested objects kept nested.
/// NEGATIVE CONTROL: mapping `Field::Absent` to `PlaceValue::Text("-")` gives the hotspot's
/// unmeasured internet time a `-` string, and this test fails at "an unknown value is not_sampled".
#[test]
fn sys_carries_the_places_line_and_object_with_unknowns_not_sampled() {
    use aterm_vitals::PlaceValue;
    let place = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let words = words_at(SINCE_MS + 60_000);
    let view = sys_view_of(&place, &words);
    assert_eq!(view.line, aterm_netplace::words::line(&place, &words));
    assert_eq!(
        view.fields.first(),
        Some(&("state", PlaceValue::Text("remote".to_owned())))
    );
    let Some((_, PlaceValue::Object(internet))) =
        view.fields.iter().find(|(key, _)| *key == "internet")
    else {
        panic!("the internet object: {:?}", view.fields);
    };
    assert!(
        internet.contains(&("checked_ms", PlaceValue::Absent("not_sampled"))),
        "an unknown value is not_sampled: {internet:?}"
    );
}

/// With a current, history-ready observer, the network band leads with the place away from the
/// ALab LAN (`Away from ALab LAN · phone
/// hotspot · 41 known`, short `Away · hotspot`), and on the ALab LAN, before any place was read,
/// while checking (learning) and with no fleet to learn a home from, the observer's own words
/// (Commons' reachable count) stand.
/// NEGATIVE CONTROLS: leaving `summary_at`'s text as the observer's whatever the place gives the
/// hotspot the observer's words, and this test fails at "away, the place's words lead"; the
/// first caption rule (any place but the ALab LAN) replaces them while learning, and
/// "Checking(Learning): the observer's words stand" fails.
#[test]
fn the_network_band_leads_with_the_place_away_from_the_alab_lan() {
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.network.visibility = aterm_types::network_state::Visibility::Active;
    snapshot.network.history = aterm_types::network_state::HistoryState::Ready;
    let observer = crate::network_deck::summary_at(&snapshot, None);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let caption = crate::network_deck::place_caption(Some(&away)).expect("away words");
    let (long, short) = (caption.long, caption.short);
    assert_eq!(
        crate::network_deck::summary_at(&snapshot, Some(&away)),
        long,
        "away, the place's words lead"
    );
    assert_ne!(long, observer);
    assert!(short.len() < long.len(), "{short} / {long}");
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    assert_eq!(crate::network_deck::place_caption(Some(&home)), None);
    assert_eq!(
        crate::network_deck::summary_at(&snapshot, Some(&home)),
        observer
    );
    for (at, alab) in [
        (
            aterm_netplace::Where::Checking(aterm_netplace::Checking::Learning),
            aterm_netplace::Alab::Learning,
        ),
        (
            aterm_netplace::Where::Network(aterm_netplace::Kind::Lan),
            aterm_netplace::Alab::NoFleet,
        ),
    ] {
        let unlearned = aterm_netplace::Place {
            at,
            alab,
            ..home.clone()
        };
        assert_eq!(
            crate::network_deck::summary_at(&snapshot, Some(&unlearned)),
            observer,
            "{at:?}: the observer's words stand"
        );
    }
}

/// PAINT READS THE CACHED PLACE AND ASKS FOR NO READ (the structural change c0ec13186b made, as
/// behaviour): the place words a resource frame captures for the network deck
/// (`network_deck::capture_place_caption`), the deck's paint of them on the render thread, and
/// the stats lens's summary take the place as last read ([`cached`]), never [`latest`], which
/// starts a read thread once the last read is stale; the hotspot's words in the paint and the
/// summary show that they read the place all the same.
/// NEGATIVE CONTROL: the frame's capture taking `crate::net_place::latest()` (as the paint did
/// before c0ec13186b) asks for a read, and this test fails at "the frame's capture asks for no
/// read".
#[test]
fn paint_reads_the_cached_place_and_asks_for_no_read() {
    let mut snapshot = aterm_types::network_state::NetworkStateSnapshot::default();
    snapshot.network.visibility = aterm_types::network_state::Visibility::Active;
    snapshot.network.history = aterm_types::network_state::HistoryState::Ready;
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let caption = crate::network_deck::place_caption(Some(&away)).expect("away words");
    let (long, short) = (caption.long, caption.short);
    hold(Some(away));
    let g = crate::stats_deck::DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
    let before = reads_asked();
    let caption = crate::network_deck::capture_place_caption();
    assert_eq!(
        reads_asked(),
        before,
        "the frame's capture asks for no read"
    );
    let (prims, _) = crate::network_deck::paint_with_inks(
        &g,
        170.0,
        &snapshot,
        &crate::lab_deck::LabDeck::default(),
        None,
        caption.as_ref(),
        &inks,
        false,
    );
    assert!(
        prims.iter().any(|prim| matches!(prim,
            crate::widget::DrawPrim::Text { s, .. } if *s == long || *s == short)),
        "paint says the held place"
    );
    assert_eq!(reads_asked(), before, "paint asks for no read");
    assert_eq!(crate::network_deck::summary(&snapshot), long);
    assert_eq!(reads_asked(), before, "nor does the summary it paints");
    let _ = latest();
    assert_eq!(
        reads_asked(),
        before + 1,
        "where a latest() is one read asked"
    );
    hold(None);
}

/// A harness's internet answer (a turn's real reply, an API probe's measure) is noted into the
/// place's evidence with its source, so the internet axis comes from a measurement that runs in
/// normal use; a local proxy's answer says nothing past this Mac and is noted nowhere.
/// A proxy named the way a LAN names its hosts (`m22`, `proxy.lan`) is a local proxy too.
/// NEGATIVE CONTROLS: noting whatever the host (dropping `note_internet`'s `host_is_internet`
/// return) notes the proxy's reply, and this test fails at "a local proxy's answer says nothing
/// of the internet"; counting every name as the internet (the old `Err(_) => true`) notes
/// `m22`'s and fails there too.
#[cfg(unix)]
#[test]
fn harness_internet_answers_are_noted_and_a_local_proxys_are_not() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    for proxy in [
        "127.0.0.1",
        "localhost",
        "192.168.4.2",
        "[::1]",
        "100.64.0.9",
        "m22",
        "proxy.lan",
        "Gateway.Home.Arpa.",
        "mac-11.local",
    ] {
        note_internet(
            true,
            InternetSource::TurnReply,
            proxy,
            aterm_netplace::unix_ms_now(),
        );
    }
    assert!(
        aterm_netplace::heard::read(&dir).internet.is_empty(),
        "a local proxy's answer says nothing of the internet"
    );
    let now = aterm_netplace::unix_ms_now();
    note_internet(true, InternetSource::TurnReply, "api.anthropic.com", now);
    note_internet(false, InternetSource::ApiProbe, "api.anthropic.com", now);
    hold_evidence_dir(None);
    let mut noted: Vec<(bool, InternetSource, String)> = aterm_netplace::heard::read(&dir)
        .internet
        .into_iter()
        .map(|note| (note.ok, note.source, note.endpoint))
        .collect();
    noted.sort_by_key(|(ok, ..)| *ok);
    assert_eq!(
        noted,
        [
            (
                false,
                InternetSource::ApiProbe,
                "api.anthropic.com".to_owned()
            ),
            (
                true,
                InternetSource::TurnReply,
                "api.anthropic.com".to_owned()
            ),
        ]
    );
}

/// A turn's reply is noted at the time it came, not when the turn-end loop booked it, so a reply
/// from before a move counts for no stay after it (the place reads only notes in its stay).
/// NEGATIVE CONTROL: stamping the note now (the old `aterm_netplace::unix_ms_now()` in place of
/// `at_ms`) fails "noted at the reply's own time".
#[cfg(unix)]
#[test]
fn a_turns_reply_is_noted_at_its_own_time() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    let came = aterm_netplace::unix_ms_now() - 30_000;
    note_internet(true, InternetSource::TurnReply, "api.anthropic.com", came);
    hold_evidence_dir(None);
    let noted: Vec<u64> = aterm_netplace::heard::read(&dir)
        .internet
        .iter()
        .map(|note| note.at_ms)
        .collect();
    assert_eq!(noted, [came], "noted at the reply's own time");
}

/// A turn's reply older than [`TURN_REPLY_FRESH_MS`] when booked (a relaunch or a wake booking a
/// session's last reply from home, hours ago) is noted nowhere; an API probe's measure is always
/// noted, at the time it was made.
/// NEGATIVE CONTROL: dropping the age rule notes the hours-old reply, and "an hours-old reply is
/// noted nowhere" fails.
#[cfg(unix)]
#[test]
fn an_hours_old_turn_reply_is_noted_nowhere() {
    use aterm_netplace::InternetSource;
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    hold_evidence_dir(Some(dir.clone()));
    let now = aterm_netplace::unix_ms_now();
    note_internet(
        true,
        InternetSource::TurnReply,
        "api.anthropic.com",
        now - 3 * 60 * 60_000,
    );
    hold_evidence_dir(None);
    assert!(
        aterm_netplace::heard::read(&dir).internet.is_empty(),
        "an hours-old reply is noted nowhere"
    );
}

fn away_entries_in(app: &crate::App) -> usize {
    let prefix = aterm_messages::exceptions::Family::NetworkPlace.prefix();
    app.messages
        .digest()
        .entries
        .iter()
        .filter(|entry| entry.key.starts_with(&prefix))
        .count()
}

fn alerts_in(app: &crate::App) -> usize {
    app.messages
        .live_rows()
        .filter(|live| matches!(live.msg.need(), aterm_messages::need::Need::Now(_)))
        .count()
}

fn settle(app: &mut crate::App) {
    if let Some(due) = app.messages_deadline() {
        let _ = app.settle_messages(due + std::time::Duration::from_secs(1));
    }
}

/// BEING AWAY IS ONE FACT FOR LATER: while this Mac is on a phone hotspot away from the ALab
/// LAN the exceptions digest holds one `network.place` entry and no alert is up; back home the
/// entry is gone. A first run that has learned no ALab LAN raises nothing.
/// NEGATIVE CONTROL: `away_entries` answering none whatever the place (its `let Some(away)`
/// arm made to return early) leaves the digest empty on the hotspot, and "one fact while away"
/// fails.
#[test]
fn the_away_entry_stands_while_away_and_ends_on_return_with_no_alert() {
    let mut app = crate::App::headless_for_test();
    let words = words_at(SINCE_MS + 10 * 60_000);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    app.state_network_place(Some(&away), &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 1, "one fact while away");
    assert_eq!(alerts_in(&app), 0, "no alert for being away");
    let learning = aterm_netplace::Place {
        alab: aterm_netplace::Alab::Learning,
        ..away.clone()
    };
    app.state_network_place(Some(&learning), &words);
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0, "a first run raises nothing");
    app.state_network_place(Some(&away), &words);
    app.state_network_place(
        Some(&aterm_netplace::sample::home(SINCE_MS + 11 * 60_000, 3)),
        &words,
    );
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0, "gone once back");
    assert_eq!(alerts_in(&app), 0);
}

/// The follow lane restates the entry when its fact moves (away begins, past the blip; the
/// network it is away on changes; it ends), not when only the internet's word does.
#[test]
fn the_follow_lane_marks_a_change_of_the_away_fact_alone() {
    let early = words_at(SINCE_MS + 1_000);
    let later = words_at(SINCE_MS + 10 * 60_000);
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let mut following = FollowState::default();
    assert!(
        following.observe(&away, early.now_ms),
        "the first publication clears any old entry"
    );
    assert_eq!(following.stated, Some(None), "a blip states nothing");
    assert!(!following.observe(&away, early.now_ms));
    assert!(following.observe(&away, later.now_ms));
    let stated = following.stated;
    assert!(stated.flatten().is_some());
    let mut answering = away.clone();
    answering.internet.state = aterm_netplace::InternetState::Ok;
    assert!(!following.observe(&answering, later.now_ms));
    assert_eq!(following.stated, stated);
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    assert!(following.observe(&home, later.now_ms));
    assert_eq!(following.stated, Some(None));
    assert!(!following.observe(&home, later.now_ms));
}

#[test]
fn the_follower_schedules_the_dwell_entry_without_repeating_a_due_wait() {
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let due = SINCE_MS + aterm_netplace::words::AWAY_ENTRY_AFTER_MS;
    let mut following = FollowState::default();
    let mut app = crate::App::headless_for_test();
    assert!(following.observe(&away, due - 1));
    assert_eq!(following.wait(due - 1), std::time::Duration::from_millis(1));
    app.state_network_place(Some(&away), &words_at(due - 1));
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0);
    // Time spent publishing is not added back to the absolute deadline.
    let mut delayed = FollowState::default();
    delayed.observe(&away, due - 1);
    assert_eq!(delayed.wait(due + 5), std::time::Duration::ZERO);
    assert!(delayed.observe(&away, due + 5));
    assert_eq!(delayed.wait(due + 5), FOLLOW_EVERY);
    assert!(following.observe(&away, due));
    app.state_network_place(Some(&away), &words_at(due));
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 1);
    assert_eq!(alerts_in(&app), 0);
    assert_eq!(following.wait(due), FOLLOW_EVERY);
    assert!(
        !following.observe(&away, due + 1),
        "no repeated key notification"
    );
}

#[test]
fn the_follower_keeps_its_external_fact_cap_at_clock_extremes_and_fixed_states() {
    let mut away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let mut following = FollowState::default();
    following.observe(&away, SINCE_MS);
    assert_eq!(following.wait(SINCE_MS), FOLLOW_EVERY);
    assert_eq!(
        following.wait(0),
        FOLLOW_EVERY,
        "rollback still resamples facts"
    );
    away.since_ms = Some(u64::MAX);
    following.observe(&away, u64::MAX - 1);
    assert_eq!(
        following.wait(u64::MAX),
        FOLLOW_EVERY,
        "no representable dwell"
    );
    away.since_before = true;
    following.observe(&away, u64::MAX);
    assert!(following.stated.flatten().is_some());
    assert_eq!(following.wait(u64::MAX), FOLLOW_EVERY);
    away.once_alab = true;
    following.observe(&away, u64::MAX);
    assert_eq!(following.stated, Some(None));
    assert_eq!(following.wait(u64::MAX), FOLLOW_EVERY);
}
