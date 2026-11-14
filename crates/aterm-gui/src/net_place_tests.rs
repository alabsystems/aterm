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
        worker_desk.run(true, || {
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
            (Arc::new(place), None)
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

/// The place's kind is netplace's own `Kind`, typed for the Machine row's glyph (A4 §8): the
/// hotspot sample a hotspot, the home LAN a LAN, an offline place offline whatever kind it held,
/// and a place with no kind still checking.
/// NEGATIVE CONTROL: reading the kind before the offline arm (`(_, Some(Kind::Lan))` first)
/// draws the offline home as a LAN, and this test fails at "offline wins".
#[test]
fn the_places_kind_is_netplaces_own() {
    use aterm_vitals::PlaceKind;
    let hotspot = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    assert_eq!(place_kind(&hotspot), PlaceKind::Hotspot);
    assert_eq!(
        sys_view_of(&hotspot, &words_at(SINCE_MS)).kind,
        PlaceKind::Hotspot
    );
    let home = aterm_netplace::sample::home(SINCE_MS, 4);
    assert_eq!(place_kind(&home), PlaceKind::Lan);
    let mut offline = home.clone();
    offline.at = aterm_netplace::Where::Offline;
    assert_eq!(place_kind(&offline), PlaceKind::Offline, "offline wins");
    let mut checking = home;
    checking.kind = None;
    assert_eq!(place_kind(&checking), PlaceKind::Checking);
}

/// The menu-bar menu's first line names the internet only where it was measured down: an
/// internet that answers, one nothing measured yet and one nothing says anything of add no
/// word to the place. NEGATIVE CONTROL: naming every state but `Ok` gives a Mac that has just
/// started `internet unknown` in its menu.
#[test]
fn the_identity_line_names_the_internet_only_where_it_is_down() {
    use aterm_netplace::InternetState;
    let mut place = aterm_netplace::sample::home(SINCE_MS, 4);
    let at = words::where_words(&place);
    for state in [
        InternetState::Ok,
        InternetState::PathUp,
        InternetState::Unknown,
    ] {
        place.internet.state = state;
        assert_eq!(identity_words_of(&place), at, "{state:?}");
    }
    place.internet.state = InternetState::Down;
    let down = identity_words_of(&place);
    assert_eq!(
        down.strip_prefix(at.as_str())
            .map(|rest| rest.ends_with(words::internet_short(&place))),
        Some(true),
        "{down}"
    );
    assert_ne!(down, at);
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
        following.observe(&away, None, early.now_ms),
        "the first publication clears any old entry"
    );
    assert_eq!(following.stated, Some(None), "a blip states nothing");
    assert!(!following.observe(&away, None, early.now_ms));
    assert!(following.observe(&away, None, later.now_ms));
    let stated = following.stated;
    assert!(stated.flatten().is_some());
    let mut answering = away.clone();
    answering.internet.state = aterm_netplace::InternetState::Ok;
    assert!(!following.observe(&answering, None, later.now_ms));
    assert_eq!(following.stated, stated);
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    assert!(following.observe(&home, None, later.now_ms));
    assert_eq!(following.stated, Some(None));
    assert!(!following.observe(&home, None, later.now_ms));
}

#[test]
fn the_follower_schedules_the_dwell_entry_without_repeating_a_due_wait() {
    let away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let due = SINCE_MS + aterm_netplace::words::AWAY_ENTRY_AFTER_MS;
    let mut following = FollowState::default();
    let mut app = crate::App::headless_for_test();
    assert!(following.observe(&away, None, due - 1));
    assert_eq!(
        following.wait(due - 1),
        Some(std::time::Duration::from_millis(1))
    );
    app.state_network_place(Some(&away), &words_at(due - 1));
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 0);
    // Time spent publishing is not added back to the absolute deadline.
    let mut delayed = FollowState::default();
    delayed.observe(&away, None, due - 1);
    assert_eq!(delayed.wait(due + 5), Some(std::time::Duration::ZERO));
    assert!(delayed.observe(&away, None, due + 5));
    assert_eq!(delayed.wait(due + 5), None);
    assert!(following.observe(&away, None, due));
    app.state_network_place(Some(&away), &words_at(due));
    settle(&mut app);
    assert_eq!(away_entries_in(&app), 1);
    assert_eq!(alerts_in(&app), 0);
    assert_eq!(following.wait(due), None);
    assert!(
        !following.observe(&away, None, due + 1),
        "no repeated key notification"
    );
}

/// The away key's own deadline is its dwell; with none representable (clock extremes, fixed
/// states) it has none, and the follower waits for a change of the place's inputs alone, on no
/// clock (it read every ten seconds, ATERM-KI-1105-20).
#[test]
fn the_follower_has_no_deadline_of_its_own_at_clock_extremes_and_fixed_states() {
    let mut away = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    let mut following = FollowState::default();
    following.observe(&away, None, SINCE_MS);
    assert_eq!(
        following.wait(SINCE_MS),
        Some(std::time::Duration::from_millis(
            aterm_netplace::words::AWAY_ENTRY_AFTER_MS
        )),
        "the dwell is the one deadline"
    );
    away.since_ms = Some(u64::MAX);
    following.observe(&away, None, u64::MAX - 1);
    assert_eq!(following.wait(u64::MAX), None, "no representable dwell");
    away.since_before = true;
    following.observe(&away, None, u64::MAX);
    assert!(following.stated.flatten().is_some());
    assert_eq!(following.wait(u64::MAX), None);
    away.once_alab = true;
    following.observe(&away, None, u64::MAX);
    assert_eq!(following.stated, Some(None));
    assert_eq!(following.wait(u64::MAX), None);
}

/// A WRITE WHILE THE FOLLOWER READS IS READ AGAIN: a change of the place's files told during a
/// read (after the owner took the last one, before it waits again) is latched, so the owner reads
/// once more as that read lands, though, watched and with no dwell pending, it waits on no clock.
/// NEGATIVE CONTROL: taking the change after the read instead of as it begins (`held.files =
/// false` beside `held.read_at = …`) loses the one told mid-read, and "the change told during
/// the second read is read again" fails at the hang detector.
#[cfg(unix)]
#[test]
fn a_file_change_during_the_followers_read_is_read_again() {
    use std::sync::mpsc;
    use std::time::Duration;
    let hang = Duration::from_secs(60);
    let desk = Arc::new(ObservationDesk::new());
    let (notice, _notices) = mpsc::channel();
    desk.register(Notice::Test(notice));
    let (read, reads) = mpsc::channel();
    let worker_desk = Arc::clone(&desk);
    let worker = std::thread::spawn(move || {
        let mut count = 0;
        worker_desk.run(true, || {
            count += 1;
            if count == 2 {
                worker_desk.input_changed();
            }
            let _ = read.send(count);
            (Arc::new(aterm_netplace::sample::home(SINCE_MS, 3)), None)
        });
    });
    let first = reads.recv_timeout(hang);
    desk.input_changed();
    let second = reads.recv_timeout(hang);
    let third = reads.recv_timeout(hang);
    desk.close();
    worker.join().unwrap();
    assert_eq!(first, Ok(1), "the first read needs no change");
    assert_eq!(second, Ok(2), "a change at rest is read");
    assert_eq!(
        third,
        Ok(3),
        "the change told during the second read is read again"
    );
}

/// The follower's cost at rest, measured: the actual observation owner over a scratch place
/// folder (as the window starts it, with the folder and the routes watched as `follow` watches
/// them), its
/// reads counted, and this process's interrupt wakeups (`proc_pid_rusage`) over a span.
/// `ATERM_MEASURE_REST_SECS` sets the span (default 65). A manual measurement, never a timing
/// gate (ATERM-KI-1105-20).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual idle measurement of the follower at rest, not a timing gate"]
fn follower_rest_wakeups() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    fn wakeups() -> u64 {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: a V4 buffer of the right size for this process's own usage.
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as i32,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: proc_pid_rusage filled it.
        unsafe { info.assume_init() }.ri_interrupt_wkups
    }
    let span = Duration::from_secs(
        std::env::var("ATERM_MEASURE_REST_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(65),
    );
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().to_owned();
    let dir = aterm_netplace::record::dir(&state);
    aterm_netplace::record::write(&dir, &aterm_netplace::record::Written::default()).unwrap();
    let desk: &'static ObservationDesk = Box::leak(Box::new(ObservationDesk::new()));
    let (notice, _notices) = std::sync::mpsc::channel();
    desk.register(Notice::Test(notice));
    let reads: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let worker = std::thread::spawn(move || {
        let files = watch_files(desk, &state);
        let routes = watch_routes(desk, aterm_uds::network::RouteChanges::open());
        observe(desk, &state, files, routes, || {
            reads.fetch_add(1, Ordering::SeqCst);
            aterm_netplace::keeper::read_projected(&state)
        });
    });
    std::thread::sleep(Duration::from_secs(2));
    let (wake0, reads0) = (wakeups(), reads.load(Ordering::SeqCst));
    std::thread::sleep(span);
    let (wake1, reads1) = (wakeups(), reads.load(Ordering::SeqCst));
    desk.close();
    worker.join().unwrap();
    eprintln!(
        "follower at rest: span_s={} reads={} interrupt_wkups={}",
        span.as_secs(),
        reads1 - reads0,
        wake1 - wake0
    );
}

/// THE FOLLOWER'S CLOCKS ARE THE KEEPER'S, NO MORE: with nothing requested and no input changed,
/// it reads again at once where it never read; on no clock where its inputs are watched and the
/// place it read is settled; at the place's own next time-only change (no file need change when
/// only the time does: an internet answer going stale); on the keeper's bounded look while a
/// move settles (the gateway's ARP entry, which no event announces, so the window is not left on
/// `checking` when no keeper keeps); and on the slow look once a watch it relied on is lost,
/// which also holds against a later start of the owner's loop.
/// NEGATIVE CONTROL: the owner's old watched arm (no look once watched, whatever the place)
/// fails "a settling move is looked at on the keeper's ARP look"; dropping `place_due` from
/// `FollowState::observe` fails "the place's own time-only change"; `run` setting `watched`
/// without `!held.lost` fails "a watch lost before the loop starts stays lost".
#[cfg(unix)]
#[test]
fn the_followers_clocks_are_the_keepers_bounded_looks_and_the_places_own_deadline() {
    use aterm_netplace::place::Checking;
    use std::time::Duration;
    let desk = ObservationDesk::new();
    let now = Instant::now();
    let home = aterm_netplace::sample::home(SINCE_MS, 3);
    {
        let mut held = desk.state.lock_or_recover();
        assert_eq!(
            held.until_read(now, SINCE_MS),
            Some(Duration::ZERO),
            "never read: at once"
        );
        held.place = Some(Arc::new(home.clone()));
        held.read_at = Some(now);
        held.watched = true;
        held.following.observe(&home, None, SINCE_MS);
        assert_eq!(
            held.until_read(now, SINCE_MS),
            None,
            "watched and settled: no clock"
        );
        held.following
            .observe(&home, Some(SINCE_MS + 5_000), SINCE_MS);
        assert_eq!(
            held.until_read(now, SINCE_MS),
            Some(Duration::from_secs(5)),
            "the place's own time-only change"
        );
        held.following.observe(&home, None, SINCE_MS);
        let mut moved = home.clone();
        moved.at = aterm_netplace::Where::Checking(Checking::Moved);
        held.place = Some(Arc::new(moved));
        assert_eq!(
            held.until_read(now, SINCE_MS),
            Some(aterm_netplace::keeper::MOVED_EVERY),
            "a settling move is looked at on the keeper's ARP look"
        );
        held.place = Some(Arc::new(home));
    }
    desk.unwatched();
    assert_eq!(
        desk.state.lock_or_recover().until_read(now, SINCE_MS),
        Some(aterm_netplace::keeper::UNWATCHED_EVERY),
        "a lost watch: the slow look"
    );
    let (read, reads) = std::sync::mpsc::channel();
    let desk = Arc::new(desk);
    desk.state.lock_or_recover().read_at = None;
    desk.request();
    let worker_desk = Arc::clone(&desk);
    let worker = std::thread::spawn(move || {
        worker_desk.run(true, || {
            let _ = read.send(());
            (Arc::new(aterm_netplace::sample::home(SINCE_MS, 3)), None)
        });
    });
    let first = reads.recv_timeout(Duration::from_secs(60));
    let watched = desk.state.lock_or_recover().watched;
    desk.close();
    worker.join().unwrap();
    assert!(first.is_ok());
    assert!(!watched, "a watch lost before the loop starts stays lost");
}

/// A ROUTE CHANGE WITH NO FILE WRITTEN IS READ AGAIN: the owner as the window starts it, its
/// folder ([`watch_files`]) and its routes ([`watch_routes`], over a stand-in route socket)
/// watched. A VPN raised within one remote stay changes the place's kind (`remote · hotspot` to
/// `remote · vpn`) but not the keeper's record, so no file of the folder changes; the route
/// socket's change alone makes the owner read again, and the cached place is the new one. The
/// file watch's first looks (one per file, each of which always tells) are taken before the
/// owner starts, so no read but the route change's can follow the first.
/// NEGATIVE CONTROL: `watch_routes` arming with a `changed` that tells nothing (`|| {}` for
/// `|| desk.input_changed()`) hangs "the route change is read again" at the detector.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_route_change_with_no_file_written_is_read_again() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    let hang = Duration::from_secs(60);
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().to_owned();
    aterm_netplace::record::write(
        &aterm_netplace::record::dir(&state),
        &aterm_netplace::record::Written::default(),
    )
    .unwrap();
    let mut lan = aterm_netplace::sample::hotspot(SINCE_MS, SINCE_MS);
    // This transition owes a notice even when the symbolic fixture time is in the future.
    lan.since_before = true;
    let mut vpn = lan.clone();
    vpn.at = aterm_netplace::Where::Remote(aterm_netplace::Kind::Vpn);
    vpn.kind = Some(aterm_netplace::Kind::Vpn);
    vpn.tunnel = Some("utun4".to_owned());
    assert_ne!(
        aterm_netplace::words::away_key(&lan, 0).expect("the old stay is settled"),
        aterm_netplace::words::away_key(&vpn, 0).expect("the new stay is settled"),
        "the route change owes a publication notice independently of the wall clock"
    );
    let desk: &'static ObservationDesk = Box::leak(Box::new(ObservationDesk::new()));
    let (notice, _notices) = std::sync::mpsc::channel();
    desk.register(Notice::Test(notice));
    let files = watch_files(desk, &state);
    let looks = files
        .as_ref()
        .map_or(0, aterm_netplace::watch::Watch::first_looks);
    let first_look = {
        // No owner runs yet: the test alone waits on the desk's wake for every first look's tell.
        let deadline = Instant::now() + hang;
        let mut held = desk.state.lock_or_recover();
        while held.told < looks && Instant::now() < deadline {
            held = desk
                .wake
                .wait_timeout(held, deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        held.told == looks && std::mem::take(&mut held.input)
    };
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let routes = watch_routes(
        desk,
        Ok(aterm_uds::network::RouteChanges::over(heard.into())),
    );
    let armed = (files.is_some(), routes.is_some());
    let (read, reads) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let count: &'static AtomicUsize = Box::leak(Box::new(AtomicUsize::new(0)));
    let (worker_lan, worker_vpn) = (lan.clone(), vpn.clone());
    let worker = std::thread::spawn(move || {
        observe(desk, &state, files, routes, || {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = read.send(n);
            if n == 2 {
                // Entering the callback does not publish its result. Hold it here so the
                // test observes that boundary independently of the worker's scheduling.
                let _ = released.recv_timeout(hang);
            }
            if n == 1 {
                (worker_lan.clone(), None)
            } else {
                (worker_vpn.clone(), None)
            }
        });
    });
    let first = reads.recv_timeout(hang);
    let sent = kernel.send(
        &[24_u32, 1, 1, 1, 37, 1]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect::<Vec<_>>(),
    );
    let after = reads.recv_timeout(hang);
    let watched = desk.state.lock_or_recover().watched;
    let before_publication = desk.state.lock_or_recover().place.clone();
    let _ = release.send(());
    let deadline = Instant::now() + hang;
    let place = {
        let mut held = desk.state.lock_or_recover();
        while held.place.as_deref() != Some(&vpn) && Instant::now() < deadline {
            held = desk
                .wake
                .wait_timeout(held, deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        held.place.clone()
    };
    desk.close();
    worker.join().unwrap();
    assert!(first_look, "the folder watch's first look tells");
    assert_eq!(armed, (true, true), "the folder and the routes are watched");
    assert!(watched);
    assert_eq!(first, Ok(1));
    assert!(sent.is_ok());
    assert_eq!(after, Ok(2), "the route change is read again");
    assert_eq!(
        before_publication.as_deref(),
        Some(&lan),
        "callback entry still holds the previous published place"
    );
    assert_eq!(
        place.as_deref(),
        Some(&vpn),
        "the cached place is the new one"
    );
}

/// A WALL MOVE IS AN INPUT CHANGE FOR THE WINDOW'S FOLLOWER: an announced wake or clock step
/// heard through the follower's wall watch (`watch_wall`) latches an input change on its desk, so
/// the owner steps again at the wall's new now instead of at the next network change
/// (ATERM-KI-1105-20).
/// NEGATIVE CONTROL: `watch_wall` arming with a `moved` that tells nothing (`|| {}` for
/// `|| desk.input_changed()`) hangs "the move is told" at the detector.
#[cfg(unix)]
#[test]
fn a_wall_move_is_an_input_change_for_the_follower() {
    let hang = std::time::Duration::from_secs(60);
    let desk: &'static ObservationDesk = Box::leak(Box::new(ObservationDesk::new()));
    assert!(watch_wall(desk, None).is_none(), "no source, no watch");
    let source = Arc::new(aterm_uds::wall_watch::WallWatch::by_hand().expect("doorbell"));
    let watch = watch_wall(desk, Some(Arc::clone(&source))).expect("the watch starts");
    source.tell(aterm_uds::wall_watch::Moved {
        stepped: true,
        ..Default::default()
    });
    let told = {
        let deadline = Instant::now() + hang;
        let mut held = desk.state.lock_or_recover();
        while held.told == 0 && Instant::now() < deadline {
            held = desk
                .wake
                .wait_timeout(held, deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        (held.told, std::mem::take(&mut held.input))
    };
    drop(watch);
    assert_eq!(
        told,
        (1, true),
        "the move is told, latched as an input change"
    );
}
