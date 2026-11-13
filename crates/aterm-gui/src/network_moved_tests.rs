// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn updater_gets_its_own_settled_network_changes_without_stealing_other_consumers() {
    let moved = Arc::new(Moved::spread_by(Duration::from_secs(30)));
    let probe = updater_probe(&moved);
    let mut other = moved.watch();
    assert!(!probe());
    let future = Instant::now() + Duration::from_secs(60);
    moved.tell_at(Woke::Route, future);
    assert!(!probe(), "the updater respects the network settling delay");
    moved.tell_at(Woke::Place, Instant::now() - Duration::from_secs(60));
    assert!(probe());
    assert!(!probe(), "one check per settled move");
    assert_eq!(
        other.take(),
        Some(Woke::Route),
        "its pending route was not consumed"
    );
}

/// One minute: a wait for something that must happen.
pub(crate) const HANG: Duration = Duration::from_secs(60);

/// verify measure, its GUI smoke and the perf arena start aterm in a scratch world
/// ([`aterm_types::world`]), often a release build that reads no development seam. Its
/// marker and its lanes-off tables turn off, by the product's own gates, every launch lane
/// that can reach the local network (a world's use of it would reach the person's real LAN
/// and its members) or the login's launchd domain: this process's `_alab._tcp` browse, the
/// converge runner's window lane, the browse's own `[swarm]` switch, fleet reads and the
/// keeper job, besides the update and package lanes. Each gate is first shown open without the world, so none passes by being
/// shut anyway.
/// NEGATIVE CONTROL: a lanes-off text without its `[swarm]` table leaves the browse's switch
/// and fleet reads on.
#[test]
fn a_scratch_world_starts_no_lane_that_reaches_the_local_network() {
    let none = |_: &str| false;
    let world = |name: &str| name == aterm_types::world::MARKER;
    assert!(browse_runs(&none), "the browse runs outside a world");
    assert!(!browse_runs(&world), "the world's marker skips the browse");
    assert_eq!(crate::converge_lane::runs(false, false, &none), cfg!(unix));
    assert!(
        !crate::converge_lane::runs(false, false, &world),
        "the world's marker skips the converge runner's window lane"
    );

    let open = crate::app_config::Config::parse("").expect("an empty config");
    assert!(open.swarm_enabled() && open.fleet_watch_enabled());
    assert_eq!(open.keeper_setting(), None);
    assert!(crate::app_config::update_checks_automatic(&open));
    assert!(open.packages_enabled());
    let empty = aterm_toml::from_str::<aterm_toml::Value>("").expect("an empty document");
    assert_eq!(aterm_secrets::swarm::enabled_in_value(&empty), Ok(true));
    let off = aterm_types::world::LANES_OFF;
    let config = crate::app_config::Config::parse(off).expect("the lanes-off tables parse");
    assert!(!config.swarm_enabled(), "swarm participation off: {off}");
    assert!(!config.fleet_watch_enabled(), "no fleet reads: {off}");
    assert_eq!(config.keeper_setting(), Some(false), "no keeper job: {off}");
    assert!(
        !crate::app_config::update_checks_automatic(&config),
        "{off}"
    );
    assert!(!config.packages_enabled(), "{off}");
    let doc = aterm_toml::from_str::<aterm_toml::Value>(off).expect("one document");
    assert_eq!(
        aterm_secrets::swarm::enabled_in_value(&doc),
        Ok(false),
        "the browse's own switch reads the swarm off: {off}"
    );
}

/// The automatic `_alab._tcp` browse runs only while `[swarm] enabled` leaves sharing on: off
/// from the start it never browses; turned on it starts; turned off it stops, saying so once;
/// turned on again it starts again. NEGATIVE CONTROL: a keeper that reads no switch browses
/// while sharing is off.
#[cfg(target_os = "macos")]
#[test]
fn the_browse_stops_and_starts_with_the_sharing_switch() {
    use std::cell::{Cell, RefCell};
    // The switch as each read finds it: off twice, on for a browse of three looks, off for
    // two reads, on again for one more browse.
    let reads = RefCell::new(
        [
            false, false, true, true, true, true, false, false, false, true, true, false,
        ]
        .into_iter(),
    );
    let sharing = || reads.borrow_mut().next().unwrap_or(false);
    let (browses, looks, waits) = (Cell::new(0), Cell::new(0), Cell::new(0));
    let said = RefCell::new(Vec::new());
    keep_browse(
        sharing,
        || waits.set(waits.get() + 1),
        |going| {
            browses.set(browses.get() + 1);
            while going() {
                looks.set(looks.get() + 1);
            }
            browses.get() < 2
        },
        |line| said.borrow_mut().push(line.to_owned()),
    );
    assert_eq!(browses.get(), 2, "it browsed while on, twice");
    assert_eq!(
        looks.get(),
        4,
        "each browse ran only while the switch read on"
    );
    assert_eq!(waits.get(), 4, "it waited while the switch read off");
    assert_eq!(
        *said.borrow(),
        [
            BROWSE_SHARING_OFF,
            BROWSE_SHARING_ON,
            BROWSE_SHARING_OFF,
            BROWSE_SHARING_ON
        ],
        "each change said once"
    );
}

/// A move told wakes every watch once, with its source; a watch made after it was not told
/// of it, and a watch with nothing told waits out its time.
/// NEGATIVE CONTROL: a `tell` that wakes no watch fails the first wait (it reads `Timer`).
#[test]
fn a_move_wakes_each_watch_once_with_its_source() {
    let moved = Arc::new(Moved::new());
    let (mut one, mut two) = (moved.watch(), moved.watch());
    let teller = Arc::clone(&moved);
    let told = std::thread::spawn(move || teller.tell(Woke::Route));
    assert_eq!(one.wait(HANG), Woke::Route);
    told.join().expect("teller");
    assert_eq!(two.take(), Some(Woke::Route));
    assert_eq!(one.take(), None, "told once");
    assert_eq!(moved.watch().take(), None, "a later watch was not told");
    assert_eq!(one.wait(Duration::ZERO), Woke::Timer);

    #[cfg(unix)]
    {
        use aterm_uds::stop::Stop;

        let moved = Arc::new(Moved::new());
        let (mut one, mut two) = (moved.watch_at_once(), moved.watch_at_once());
        let run_stop = Stop::new();
        let generation_stop = Stop::new();
        let stops = [&run_stop, &generation_stop];
        let deadline = Instant::now() + HANG;

        // A queued event wins without needing a native waiter to have been installed.
        moved.tell(Woke::Route);
        assert_eq!(
            one.wait_until(&stops, Some(deadline)).unwrap(),
            Some(Woke::Route)
        );
        assert_eq!(
            two.wait_until(&stops, Some(deadline)).unwrap(),
            Some(Woke::Route)
        );

        // Both native waiters borrow this same generation. The real broadcast must
        // leave its descriptor ready for each, even when neither was polling yet.
        let waiting = moved.told.lock().unwrap().wake();
        let bell = waiting.bell().unwrap();
        moved.tell(Woke::Place);
        for _ in 0..2 {
            assert_eq!(
                aterm_uds::ready::any_now(&[bell]).unwrap(),
                [0],
                "a native notification is retained for every waiter"
            );
        }
        assert_eq!(
            one.wait_until(&stops, Some(deadline)).unwrap(),
            Some(Woke::Place)
        );
        assert_eq!(
            two.wait_until(&stops, Some(deadline)).unwrap(),
            Some(Woke::Place)
        );
        assert_eq!(
            one.wait_until(&stops, Some(Instant::now())).unwrap(),
            Some(Woke::Timer),
            "a consumed native generation must not spin"
        );

        // A new move does not renew the caller's original maintenance deadline.
        let due = Instant::now();
        moved.tell(Woke::Beacon);
        assert_eq!(
            one.wait_until(&stops, Some(due)).unwrap(),
            Some(Woke::Beacon)
        );
        assert_eq!(
            one.wait_until(&stops, Some(due)).unwrap(),
            Some(Woke::Timer)
        );

        // Either stop ends the same native wait without consuming a queued move.
        for run in [false, true] {
            let run_stop = Arc::new(Stop::new());
            let generation_stop = Arc::new(Stop::new());
            let mut watch = moved.watch_at_once();
            let cancelled = if run { &run_stop } else { &generation_stop };
            cancelled.set();
            moved.tell(Woke::Route);
            assert_eq!(
                watch
                    .wait_until(&[&run_stop, &generation_stop], Some(deadline))
                    .unwrap(),
                None
            );
            assert_eq!(watch.take(), Some(Woke::Route));

            let run_stop = Arc::new(Stop::new());
            let generation_stop = Arc::new(Stop::new());
            let (worker_run, worker_generation) =
                (Arc::clone(&run_stop), Arc::clone(&generation_stop));
            let mut watch = moved.watch_at_once();
            let worker = std::thread::spawn(move || {
                watch.wait_until(&[&worker_run, &worker_generation], Some(deadline))
            });
            if run {
                run_stop.set();
            } else {
                generation_stop.set();
            }
            assert_eq!(worker.join().unwrap().unwrap(), None);
        }
    }
}

/// Joining a LAN tells a route move, and the `_alab._tcp` browse or a Commons beacon tells
/// another within this Mac's spread: the two are one wake, and a watch hears it as the route
/// move, so a consumer that looks again only on a route move (the API reach probe) never
/// drops it. A watch that heard the route move already is told the later move's own source.
/// NEGATIVE CONTROL: keeping only the latest move's source wakes the watch with `Browse`.
#[test]
fn a_route_move_followed_by_a_browse_still_wakes_a_route_watch() {
    let moved = Arc::new(Moved::spread_by(Duration::from_millis(300)));
    let mut watch = moved.watch();
    let mut at_once = moved.watch_at_once();
    moved.tell(Woke::Route);
    moved.tell(Woke::Browse);
    assert_eq!(
        watch.wait(HANG),
        Woke::Route,
        "the route move is not hidden"
    );
    assert_eq!(at_once.take(), Some(Woke::Route));
    moved.tell(Woke::Beacon);
    assert_eq!(
        watch.wait(HANG),
        Woke::Beacon,
        "no route move since it heard one"
    );
    assert_eq!(at_once.take(), Some(Woke::Beacon));
}

/// A PER-MAC SPREAD AFTER A MOVE (docs/DESIGN-fluid-swarm-2026-10-28.md, F4's follow-up): on
/// a signal spread by 3 s, a move is due 3 s after it was told, never before; a second move
/// told before the first was heard is one wake, at the later one's time; a wait shorter than the spread reads
/// `Timer` and leaves the move to a later one, and a wait past it is woken by it.
/// NEGATIVE CONTROL: a `Moved` that ignores its spread (each move due at once) fails "not
/// before its spread".
#[test]
fn a_move_is_due_once_this_macs_spread_has_passed() {
    let secs = Duration::from_secs;
    let spread = secs(3);
    let moved = Arc::new(Moved::spread_by(spread));
    let mut watch = moved.watch();
    let t0 = Instant::now() + secs(3600);
    moved.tell_at(Woke::Route, t0);
    assert_eq!(watch.take_at(t0), None, "not before its spread");
    assert_eq!(
        watch.take_at(t0 + spread - Duration::from_millis(1)),
        None,
        "not before its spread"
    );
    assert_eq!(watch.take_at(t0 + spread), Some(Woke::Route));
    assert_eq!(watch.take_at(t0 + spread), None, "told once");

    moved.tell_at(Woke::Route, t0 + secs(10));
    moved.tell_at(Woke::Beacon, t0 + secs(11));
    assert_eq!(
        watch.take_at(t0 + secs(13)),
        None,
        "the later move's spread"
    );
    assert_eq!(
        watch.take_at(t0 + secs(14)),
        Some(Woke::Route),
        "told as the route move among them"
    );
    assert_eq!(watch.take_at(t0 + secs(60)), None, "one wake for both");

    let hour = Arc::new(Moved::spread_by(secs(3600)));
    let mut waiting = hour.watch();
    hour.tell(Woke::Route);
    assert_eq!(
        waiting.wait(Duration::from_millis(1)),
        Woke::Timer,
        "a wait shorter than the spread"
    );
    assert_eq!(
        waiting.take_at(Instant::now() + secs(3600)),
        Some(Woke::Route),
        "the move is left to a later wait"
    );
    let soon = Arc::new(Moved::spread_by(Duration::from_millis(5)));
    let mut waiting = soon.watch();
    soon.tell(Woke::Beacon);
    assert_eq!(waiting.wait(HANG), Woke::Beacon, "woken once it is due");
}

/// THE SPREAD IS THE HERD'S (F4's follow-up review): a Mac whose fleet holds no other member
/// (in no fleet, or alone in one) hears each move at once, since no other Mac dials with it; a
/// fleet of 5 other members spreads over 5/35ths of this Mac's offset; a herd of 35 or more
/// over all of it. A watch made at once (the API reach probe's) hears a move at once on a
/// spread signal, while a plain watch on it waits the spread out.
/// NEGATIVE CONTROL: a `herded` that keeps the whole offset whatever the herd fails "a Mac
/// alone"; a watch that waits the spread out whatever it was made as fails "at once".
#[test]
fn a_mac_with_no_herd_hears_a_move_at_once_and_a_herd_spreads_its_share() {
    let secs = Duration::from_secs;
    let t0 = Instant::now() + secs(3600);
    let alone = Arc::new(Moved::herded_by(secs(7), || 0));
    let mut watch = alone.watch();
    alone.tell_at(Woke::Route, t0);
    assert_eq!(watch.take_at(t0), Some(Woke::Route), "a Mac alone");

    let five = Arc::new(Moved::herded_by(secs(7), || 5));
    let mut watch = five.watch();
    five.tell_at(Woke::Route, t0);
    assert_eq!(
        watch.take_at(t0 + Duration::from_millis(999)),
        None,
        "5/35ths of 7 s is 1 s"
    );
    assert_eq!(watch.take_at(t0 + secs(1)), Some(Woke::Route));

    assert_eq!(herded(secs(7), 35), secs(7), "a whole herd");
    assert_eq!(herded(secs(7), 200), secs(7), "past a herd, the whole");
    assert_eq!(herded(secs(7), 0), Duration::ZERO);

    let spread = Arc::new(Moved::spread_by(secs(3)));
    let (mut plain, mut at_once) = (spread.watch(), spread.watch_at_once());
    spread.tell_at(Woke::Route, t0);
    assert_eq!(at_once.take_at(t0), Some(Woke::Route), "at once");
    assert_eq!(
        plain.take_at(t0),
        None,
        "a plain watch waits the spread out"
    );
    assert_eq!(plain.take_at(t0 + secs(3)), Some(Woke::Route));
    let mut waiting = spread.watch_at_once();
    spread.tell_at(Woke::Beacon, Instant::now() + secs(3600));
    assert_eq!(waiting.wait(HANG), Woke::Beacon, "at once, waiting");
}

/// THE OTHERS A MEMBER'S BOOK COUNTS: its book's peers and its copy of its issuer's book,
/// read only for the `Issuer` entry's node, each member once and this Mac's own node aside; a
/// peer a person named counts by its place (host and port); an empty book counts none.
/// NEGATIVE CONTROL: reading the copy for another entry with a node (the first one, or a
/// `Joined` one) fails "the copy of the issuer's book", and keying a named peer by its host
/// alone fails "each place once".
#[cfg(unix)]
#[test]
fn a_member_counts_its_issuer_and_the_issuers_book_once_each_but_never_itself() {
    use aterm_net::fleet_member::{Peer, Via};
    let peer = |node: Option<&str>, host: &str, via: Via| Peer {
        node: node.map(Into::into),
        key: None,
        key_from: None,
        host: host.into(),
        addr: None,
        port: 7100,
        version: None,
        seen_ms: None,
        via,
        fleet: None,
        signed: None,
    };
    let book = [
        peer(Some("j"), "j.local", Via::Joined),
        peer(Some("i"), "i.local", Via::Issuer),
    ];
    let copies = |issuer: &str| {
        assert_eq!(issuer, "i", "the copy of the issuer's book");
        ["x", "y", "x", "m", "j"]
            .map(|node| peer(Some(node), node, Via::IssuerBook))
            .to_vec()
    };
    assert_eq!(others_in_book(Some("m"), &book, copies), 4, "j, i, x and y");
    let named = [
        peer(None, "h", Via::Manual),
        peer(None, "g", Via::Manual),
        Peer {
            port: 7200,
            ..peer(None, "h", Via::Manual)
        },
        peer(None, "h", Via::Manual),
    ];
    assert_eq!(
        others_in_book(None, &named, |_| panic!("no issuer")),
        3,
        "each place once: h:7100, g:7100 and h:7200"
    );
    assert_eq!(others_in_book(Some("m"), &[], |_| panic!("no book")), 0);
}

/// A RETURN FROM AWAY WAITS NO HERD, A REBOOT KEEPS IT (docs/DESIGN-network-place-2026-11-01.md,
/// part B): a move told as this Mac comes back from a long stay on a phone hotspot (told
/// before the keeper read home, or after) waits none of its fleet's spread, since only it
/// moved; a router or access point reboot (the ALab LAN, offline, the same ALab LAN) and a
/// Mac with no record keep the herd of 35.
/// NEGATIVE CONTROL: a `herd_given` that keeps the book whatever the record (the original
/// `members_here`) fails "a lone return waits no herd".
#[test]
fn a_return_from_away_waits_no_herd_and_a_reboot_keeps_it() {
    use aterm_netplace::place::{Known, Record, step};
    let t0 = 1_800_000_000_000_u64;
    let min = 60_000;
    let home = aterm_netplace::sample::home(t0, 3);
    let hotspot = aterm_netplace::sample::hotspot(t0, t0);
    let facts = |place: &aterm_netplace::Place| aterm_netplace::Facts {
        route: place.route.clone(),
        tunnel: None,
        path: place.path,
    };
    let offline = aterm_netplace::Facts::default();
    let walk = |record: Record, steps: &[(&aterm_netplace::Facts, u64)]| {
        steps.iter().fold(record, |record, (facts, now)| {
            step(&record, facts, &Default::default(), None, *now, Some(t0)).0
        })
    };
    let learned = Record {
        remembered: vec![Known {
            network: home.network.clone().expect("strong"),
            learned_ms: t0,
            confirmed_ms: t0,
        }],
        ..Record::default()
    };
    let (at_home, on_hotspot) = (facts(&home), facts(&hotspot));
    let away = walk(learned.clone(), &[(&at_home, t0), (&on_hotspot, t0 + min)]);
    assert_eq!(
        herd_given(35, Some(&away), t0 + 60 * min),
        0,
        "a lone return waits no herd"
    );
    let arrived = walk(away, &[(&at_home, t0 + 60 * min)]);
    assert_eq!(
        herd_given(35, Some(&arrived), t0 + 60 * min + 5_000),
        0,
        "told once the keeper read home"
    );
    let rebooted = walk(
        learned,
        &[
            (&at_home, t0),
            (&offline, t0 + 30 * min),
            (&at_home, t0 + 31 * min),
        ],
    );
    assert_eq!(
        herd_given(35, Some(&rebooted), t0 + 31 * min),
        35,
        "a reboot keeps the herd"
    );
    assert_eq!(herd_given(35, None, t0), 35, "no record keeps the herd");
}

/// THE SETTLED PLACE IS A MOVE TOLD WITH NO REST: the place source over a record directory
/// tells each watch [`Woke::Place`] once the record settles onto a network (from offline),
/// and again at once when it settles onto another one.
/// NEGATIVE CONTROL: a source that reads no record (its closure answering `Some(false)`)
/// tells nothing, and the first wait hangs past the one-minute detector: "the arrival".
#[cfg(unix)]
#[test]
fn a_settled_place_is_told_as_a_move_with_no_rest() {
    use aterm_netplace::place::{Record, step};
    use aterm_netplace::record::{Written, write};
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = aterm_netplace::record::dir(root.path());
    let t0 = 1_800_000_000_000_u64;
    let home = aterm_netplace::sample::home(t0, 3);
    let other = {
        let mut route = home.route.clone().expect("a route");
        route.gateway_hw = Some(vec![9, 9, 9, 9, 9, 9]);
        route
    };
    let facts = |route: Option<aterm_netplace::RouteFacts>| aterm_netplace::Facts {
        route,
        tunnel: None,
        path: home.path,
    };
    let mut record = Record::default();
    let mut put = |facts: &aterm_netplace::Facts, now: u64| {
        record = step(&record, facts, &Default::default(), None, now, Some(t0)).0;
        write(
            &dir,
            &Written {
                record: record.clone(),
                ..Written::default()
            },
        )
        .expect("the record");
    };
    put(&facts(None), t0);
    let moved = Arc::new(Moved::new());
    let mut watch = moved.watch();
    let looks = PlaceLooks {
        after_a_change: Duration::from_millis(5),
        fast_for: Duration::from_millis(5),
        steady: Duration::from_millis(5),
    };
    assert!(hear_place(&moved, dir.clone(), None, looks));
    put(&facts(home.route.clone()), t0 + 60_000);
    assert_eq!(watch.wait(HANG), Woke::Place, "the arrival");
    put(&facts(Some(other)), t0 + 120_000);
    assert_eq!(watch.wait(HANG), Woke::Place, "another network, at once");
}

/// THIS MAC'S SPREAD: derived from its node id, else its host name; the same each time for
/// one Mac, inside the window, and apart for different Macs, so 35 Macs back on one LAN fill
/// the window rather than one instant; a Mac with neither has none.
/// NEGATIVE CONTROL: a constant spread fails "35 Macs land apart".
#[test]
fn each_mac_spreads_its_wakes_by_an_offset_of_its_own() {
    let node = "3fa2c1d09e8b7a6f5e4d3c2b1a090807".to_owned();
    assert_eq!(
        spread_seed(|| Some(node.clone()), || Some("m22.local".to_owned())).as_deref(),
        Some(node.as_str()),
        "the node id wins"
    );
    assert_eq!(
        spread_seed(|| None, || Some("m22.local".to_owned())).as_deref(),
        Some("m22.local"),
        "else the host name"
    );
    assert_eq!(
        spread_seed(|| Some(String::new()), || Some("m22.local".to_owned())).as_deref(),
        Some("m22.local"),
        "an empty node id is none"
    );
    assert_eq!(spread_seed(|| None, || None), None);
    assert_eq!(spread_of(None), Duration::ZERO, "no seed, no spread");
    assert_eq!(
        spread_of(Some(&node)),
        spread_of(Some(&node)),
        "the same each time"
    );
    let macs: Vec<String> = (1..=35).map(|n| format!("m{n}.local")).collect();
    let offsets: Vec<Duration> = macs.iter().map(|mac| spread_of(Some(mac))).collect();
    assert!(offsets.iter().all(|at| *at < SPREAD), "{offsets:?}");
    let distinct: std::collections::BTreeSet<_> = offsets.iter().collect();
    let seconds: std::collections::BTreeSet<_> = offsets.iter().map(Duration::as_secs).collect();
    assert!(
        distinct.len() >= 30 && seconds.len() >= 6,
        "35 Macs land apart: {} distinct offsets in {} of the window's {} seconds: {offsets:?}",
        distinct.len(),
        seconds.len(),
        SPREAD.as_secs()
    );
}

/// A source thread tells a move each time its source hears one, and nothing for what it
/// hears that is none; it ends when its source does.
/// NEGATIVE CONTROL: a source thread that drops what it heard fails the wait (`Timer`).
#[test]
fn a_source_tells_each_move_it_hears() {
    let moved = Arc::new(Moved::new());
    let mut watch = moved.watch();
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    assert!(spawn_source(
        "test-source",
        Arc::clone(&moved),
        (Woke::Beacon, Duration::ZERO),
        move || rx.recv().ok()
    ));
    tx.send(false).expect("source");
    tx.send(true).expect("source");
    assert_eq!(watch.wait(HANG), Woke::Beacon);
    drop(tx);
}

/// A peer's comings and goings on the browse are no move of this Mac's network: over an hour
/// in which, each half minute, a member starts under a name never heard (each process names
/// its advert afresh), one leaves, and one comes back after longer than
/// [`aterm_secrets::discover::SILENCE`], the source, asked each half minute as its rest
/// allows, tells none. This Mac back after every member's silence is one move. The silence
/// runs at its real length, on injected times.
/// NEGATIVE CONTROL: the rule that took each member not held as a move (0.155.0, at its rest
/// floor in the owner's log) tells a move at every half minute of the hour; the closing
/// rejoin tells its one move under either rule.
#[test]
#[cfg(unix)]
fn an_hour_of_members_coming_and_going_on_the_browse_tells_no_move() {
    let secs = Duration::from_secs;
    let silence = aterm_secrets::discover::SILENCE;
    let bell = Arc::new(aterm_secrets::wake::Bell::new());
    let browse = aterm_secrets::discover::Watch::idle_for(
        aterm_agent::node_service::ALAB_TCP_SERVICE,
        &bell,
        &["alab-0000a0a0"],
    );
    let moved = Arc::new(Moved::new());
    let (look, looks) = std::sync::mpsc::channel::<()>();
    let (answer, answers) = std::sync::mpsc::channel::<bool>();
    let (ended, end) = std::sync::mpsc::channel::<()>();
    assert!(spawn_source(
        "test-browse-churn",
        Arc::clone(&moved),
        (Woke::Browse, Duration::ZERO),
        move || {
            if looks.recv().is_err() {
                let _ = ended.send(());
                return None;
            }
            let rung = bell.nap(Duration::ZERO) == aterm_secrets::wake::Wake::Browse;
            answer.send(rung).ok()?;
            Some(rung)
        }
    ));
    let ask = || {
        look.send(()).expect("the source is running");
        answers.recv_timeout(HANG).expect("the source looks")
    };
    let name = |i: u32| format!("alab-{i:08x}");
    let start = Instant::now();
    browse.began_browse();
    browse.hear_at(true, 4, &name(0), start);
    browse.hear_at(true, 4, "alab-0000a0a0", start);
    browse.child_began(secs(3600));
    let mut at = start;
    let mut told = 0;
    for i in 1..=120 {
        at += secs(30);
        browse.hear_at(true, 4, &name(i), at);
        if i > 4 {
            browse.hear_at(false, 4, &name(i - 4), at);
        }
        if i > 9 {
            browse.hear_at(true, 4, &name(i - 9), at);
        }
        told += usize::from(ask());
    }
    assert_eq!(told, 0, "moves told over {:?} of churn", at - start);
    let members = browse.heard();
    assert!(members.len() > 100, "{members:?}");
    let left = at + secs(1);
    for member in &members {
        browse.hear_at(false, 4, member, left);
    }
    assert!(!ask(), "every member left");
    for member in &members {
        browse.hear_at(true, 4, member, left + silence + secs(1));
    }
    assert!(ask(), "back after every member's silence");
    assert!(!ask(), "told once");
    drop(look);
    end.recv_timeout(HANG)
        .expect("the source ends once nothing asks");
    assert_eq!(
        moved.told(),
        1,
        "one move for the rejoin, none for the hour"
    );
    drop(browse);
}

/// One rejoin heard on the browse is one move: when this Mac comes back after longer than
/// [`aterm_secrets::discover::SILENCE`], every member's `Add` comes at once (none is held
/// since its `Rmv` as the Mac left), and the browse's bell keeps one ring for all of them, so
/// the source tells them as one move, as the route source and the Commons beacon tell theirs,
/// not one move a rest each for a quarter of an hour.
/// NEGATIVE CONTROL: a nap that leaves the bell rung tells a move each time it is asked.
#[test]
#[cfg(unix)]
fn one_rejoin_heard_on_the_browse_is_one_move() {
    let bell = Arc::new(aterm_secrets::wake::Bell::new());
    let browse = aterm_secrets::discover::Watch::idle_for(
        aterm_agent::node_service::ALAB_TCP_SERVICE,
        &bell,
        &[],
    );
    let start = Instant::now();
    let left = start + Duration::from_secs(30);
    let back = left + aterm_secrets::discover::SILENCE + Duration::from_secs(1);
    for (add, at) in [(true, start), (false, left), (true, back)] {
        for i in 0..34 {
            browse.hear_at(add, 4, &format!("member-{i:02}"), at);
        }
    }
    let moved = Arc::new(Moved::new());
    let mut watch = moved.watch();
    let (ended, end) = std::sync::mpsc::channel();
    let mut asked = 0;
    assert!(spawn_source(
        "test-browse",
        Arc::clone(&moved),
        (Woke::Browse, Duration::ZERO),
        move || {
            asked += 1;
            let rung = bell.nap(Duration::ZERO) == aterm_secrets::wake::Wake::Browse;
            if !rung || asked > 100 {
                let _ = ended.send(());
                return None;
            }
            Some(rung)
        }
    ));
    end.recv_timeout(HANG)
        .expect("the source ends once the bell is quiet");
    assert_eq!(watch.take(), Some(Woke::Browse));
    assert_eq!(moved.told(), 1, "one move for the whole rejoin");
    drop(browse);
}

/// A Commons peer heard after every peer's silence longer than [`SILENCE`], or one new, is
/// a move once the baseline is over; one heard within it, or one gone silent, is not.
/// NEGATIVE CONTROL: a look that takes every heard peer as new fails the second look; one
/// that never forgets a peer misses its return after the silence.
#[test]
fn a_peer_heard_again_after_a_silence_is_a_move() {
    use aterm_types::network_state::Reachability::{LastSeen, Reachable};
    let at = Instant::now();
    let after = |secs| at + Duration::from_secs(secs);
    let mut heard = Heard::new(1);
    assert!(!heard.look_at(&[peer(1, Reachable)], at), "baseline");
    assert!(
        !heard.look_at(&[peer(1, Reachable)], after(2)),
        "still heard"
    );
    assert!(!heard.look_at(&[peer(1, LastSeen)], after(4)), "silent");
    let back = 2 + SILENCE.as_secs() + 1;
    assert!(
        !heard.look_at(&[peer(1, LastSeen)], after(back - 2)),
        "still silent"
    );
    assert!(heard.look_at(&[peer(1, Reachable)], after(back)), "back");
    assert!(
        heard.look_at(&[peer(1, Reachable), peer(2, Reachable)], after(back + 2)),
        "new"
    );
    assert!(
        !heard.look_at(&[peer(1, Reachable), peer(2, Reachable)], after(back + 4)),
        "both heard"
    );
}

/// The beacon source's baseline at its real count, not the one-look baseline of the tests
/// around it: the source looks once a [`BEACON_LOOK`] after it starts, and every look in
/// the first half minute only learns who is heard, one look each however many peers it
/// hears. A runtime just started has confirmed no peer yet (each is LastSeen or Unknown
/// until an authenticated contact), so the first look hears none; from the second it hears
/// two. A peer first heard on the half minute's last look was there before this process
/// looked, so it is no move, nor on the next look (heard within [`SILENCE`], as are the
/// peers heard throughout); a peer first heard on the first look after the half minute is
/// one.
/// NEGATIVE CONTROL: `SETTLE_LOOKS = 14` fails "first heard on the baseline's last look";
/// `SETTLE_LOOKS = 16` fails "new on the first look after the baseline"; a baseline used up
/// once per peer heard rather than once per look fails "first heard on the baseline's last
/// look"; one that a look hearing no peer leaves whole fails "new on the first look after
/// the baseline".
#[test]
fn the_commons_baseline_is_fifteen_looks() {
    use aterm_types::network_state::Reachability::{LastSeen, Reachable, Unknown};
    const HALF_MINUTE: Duration = Duration::from_secs(30);
    let last = u32::try_from(HALF_MINUTE.as_millis() / BEACON_LOOK.as_millis())
        .expect("a half minute is a few looks");
    assert_eq!(BEACON_LOOK * last, HALF_MINUTE, "whole looks");
    let start = Instant::now();
    let at = |look: u32| start + BEACON_LOOK * look;
    let unconfirmed = [peer(1, LastSeen), peer(4, Unknown)];
    let two = [peer(1, Reachable), peer(4, Reachable)];
    let (mut heard, mut fresh) = (Heard::commons(), Heard::commons());
    for watch in [&mut heard, &mut fresh] {
        assert!(
            !watch.look_at(&unconfirmed, at(1)),
            "baseline look 1 hears no peer"
        );
        for look in 2..last {
            assert!(!watch.look_at(&two, at(look)), "baseline look {look}");
        }
    }
    let three = [peer(1, Reachable), peer(4, Reachable), peer(2, Reachable)];
    assert!(
        !heard.look_at(&three, at(last)),
        "first heard on the baseline's last look"
    );
    assert!(
        !heard.look_at(&three, at(last + 1)),
        "heard throughout, and since the baseline's last look"
    );
    assert!(
        heard.look_at(
            &[
                peer(1, Reachable),
                peer(4, Reachable),
                peer(2, Reachable),
                peer(3, Reachable)
            ],
            at(last + 2)
        ),
        "new after the baseline"
    );
    assert!(
        !fresh.look_at(&two, at(last)),
        "heard throughout the baseline"
    );
    assert!(
        fresh.look_at(
            &[peer(1, Reachable), peer(4, Reachable), peer(3, Reachable)],
            at(last + 1)
        ),
        "new on the first look after the baseline"
    );
}

/// Missing samples do not count as empty observations, even after the silence has elapsed.
/// NEGATIVE CONTROL: treating an unavailable sample as an empty look consumes the baseline
/// and forgets the peer before an actual observation arrives.
#[test]
fn an_unavailable_commons_sample_leaves_the_watch_unchanged() {
    use aterm_types::network_state::Reachability::Reachable;
    let at = Instant::now();
    let later = at + SILENCE + Duration::from_secs(1);
    let mut heard = Heard::new(2);
    assert!(!heard.look_sample_at(Some(&[peer(1, Reachable)]), at));
    assert_eq!(heard.baseline, 1);
    let before = heard.heard.clone();

    assert!(!heard.look_sample_at(None, later));
    assert_eq!(heard.baseline, 1, "no baseline look was observed");
    assert_eq!(heard.heard, before, "no peer was forgotten or heard again");

    assert!(
        !heard.look_sample_at(Some(&[peer(1, Reachable)]), later),
        "the next actual look finishes the baseline"
    );
    assert_eq!(heard.baseline, 0);
    assert_eq!(heard.heard.get(&[1; 16]), Some(&later));
    assert!(
        heard.look_sample_at(Some(&[peer(1, Reachable), peer(2, Reachable)]), later),
        "a new peer in a subsequent actual sample is a move"
    );
}

/// A live LAN's peers drop out of Reachable and back about every half minute (an
/// authenticated contact keeps one Reachable 15 s): such a blip is no move, however often it
/// comes, so the source does not fire after each of its rests for as long as the LAN is up.
/// NEGATIVE CONTROL: a look that compares with the last look only (the peer was LastSeen
/// there) takes each blip's return as a move.
#[test]
fn a_peer_back_from_a_short_blip_is_no_move() {
    use aterm_types::network_state::Reachability::{LastSeen, Reachable};
    let mut heard = Heard::new(1);
    assert!(!heard.look(&[peer(1, Reachable)]), "baseline");
    for blip in 0..16 {
        assert!(!heard.look(&[peer(1, LastSeen)]), "silent, blip {blip}");
        assert!(!heard.look(&[peer(1, Reachable)]), "back, blip {blip}");
    }
}

/// One peer back from any silence is no move while another is heard: among some 40 Macs one
/// always is, and the source fired after each of its rests.
/// NEGATIVE CONTROL: a look that times each peer's silence alone takes this return as a move.
#[test]
fn a_peer_back_while_another_is_heard_is_no_move() {
    use aterm_types::network_state::Reachability::{LastSeen, Reachable};
    let at = Instant::now();
    let mut heard = Heard::new(1);
    let both = [peer(1, Reachable), peer(2, Reachable)];
    assert!(!heard.look_at(&both, at), "baseline");
    let back = SILENCE.as_secs() * 2;
    for secs in (2..back).step_by(2) {
        let look = at + Duration::from_secs(secs);
        let silent = [peer(1, Reachable), peer(2, LastSeen)];
        assert!(!heard.look_at(&silent, look), "peer 2 silent at {secs} s");
    }
    assert!(
        !heard.look_at(&both, at + Duration::from_secs(back)),
        "peer 1 was heard all along"
    );
}

/// The beacon reads the network's peers and asks for no services read, which would keep the
/// services' observer sampling for a beacon that draws none of them.
/// NEGATIVE CONTROL: a beacon reading with services records `WithServices`.
#[test]
fn the_beacon_reads_the_network_alone() {
    use aterm_types::network_state::{NetworkStateSnapshot, Reachability::Reachable};
    let asked = Arc::new(Mutex::new(Vec::new()));
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.peers = vec![peer(7, Reachable)];
    let snapshot = Arc::new(snapshot);
    let reads = Arc::clone(&asked);
    let peers = beacon_peers(Arc::new(move |read| {
        reads.lock().unwrap().push(read);
        Some(Arc::clone(&snapshot))
    }));
    assert_eq!(peers(), Some(vec![peer(7, Reachable)]));
    assert_eq!(*asked.lock().unwrap(), [crate::NetworkRead::Network]);
}

/// A peer with one byte of machine id, as the Commons runtime observes it.
pub(crate) fn peer(
    id: u8,
    reachability: aterm_types::network_state::Reachability,
) -> aterm_types::network_state::PeerObservation {
    aterm_types::network_state::PeerObservation {
        slot: u64::from(id),
        identity: aterm_types::network_state::MachineIdentity {
            machine_id: [id; 16],
            incarnation: [0; 16],
            label: None,
        },
        first_seen_ms: 1,
        last_seen_ms: 1,
        last_checked_ms: None,
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability,
        instances: None,
        samples: Vec::new(),
        lease_ms: None,
    }
}
