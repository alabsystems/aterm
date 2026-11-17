// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use aterm_spec::wait::MUST_HAPPEN;

/// Whether `name` is a window's relay socket's, by the node job's own rule.
fn is_relay(name: &str) -> bool {
    crate::fleet_listen::node_serve::parse_relay_filename(name).is_some()
}

/// A node's relays: the relay sockets in `dir`.
fn relays(dir: &Path) -> (&Path, fn(&str) -> bool) {
    (dir, is_relay)
}

/// Replace `path`'s contents by a rename over it, which moves its stamp once: a write in place
/// truncates first, and a watch that looks between the truncation and the write tells twice.
fn replace(path: &Path, contents: &[u8]) {
    let next = path.with_extension("next");
    std::fs::write(&next, contents).unwrap();
    std::fs::rename(&next, path).unwrap();
}

/// A ring before a waiter arms is pending at its arm, and a waiter that takes a ring before it
/// reads keeps every ring made during the read pending; one made by another thread wakes its
/// wait. Memory only.
/// NEGATIVE CONTROL: the reverse order, a read and then a take of the generation current after
/// it, loses the ring made during the read, which the test asserts: the order is take, then read.
#[test]
fn a_ring_is_never_lost_between_a_read_and_the_next_wait() {
    let bell = Arc::new(Bell::default());
    let mut seen = Seen::new(&bell);
    assert!(seen.arm().is_ok(), "nothing pending at first");
    let rung = bell.ring();
    assert_eq!(
        seen.arm().err(),
        Some(rung),
        "a ring before the arm is pending"
    );

    // Take, then read: a ring during the read stays pending.
    seen.acknowledge(rung);
    let during = bell.ring();
    assert_eq!(
        seen.arm().err(),
        Some(during),
        "the ring during the read is pending"
    );
    assert!(seen.take(), "taken");
    assert!(seen.arm().is_ok(), "nothing else pending");

    // CONTROL: read, then take the generation current after it: the read's ring is gone.
    let mut late = Seen::new(&bell);
    let _during_read = bell.ring();
    late.acknowledge(bell.generation());
    assert!(
        late.arm().is_ok(),
        "CONTROL: taking after the read loses the ring made during it"
    );

    // Two waiters on one bell: each hears a ring, and neither takes the other's.
    let mut other = Seen::new(&bell);
    let shared = bell.ring();
    assert_eq!(seen.arm().err(), Some(shared));
    assert!(seen.take());
    assert_eq!(
        other.arm().err(),
        Some(shared),
        "the other waiter still has it"
    );
    assert!(other.take());

    // A ring from another thread wakes a wait; a stop and a move end it too.
    let moved = Arc::new(crate::network_moved::Moved::new());
    let mut network = moved.watch_at_once();
    let stop = Stop::new();
    let ringer = {
        let bell = Arc::clone(&bell);
        std::thread::spawn(move || bell.ring())
    };
    assert_eq!(
        wait(
            &mut seen,
            &mut network,
            &[&stop],
            Instant::now() + MUST_HAPPEN
        )
        .unwrap(),
        Wake::Inputs
    );
    ringer.join().unwrap();
    assert!(!seen.take(), "the wait took the ring");
    moved.tell(crate::network_moved::Woke::Route);
    assert_eq!(
        wait(
            &mut seen,
            &mut network,
            &[&stop],
            Instant::now() + MUST_HAPPEN
        )
        .unwrap(),
        Wake::Moved(crate::network_moved::Woke::Route)
    );
    assert_eq!(
        wait(&mut seen, &mut network, &[&stop], Instant::now()).unwrap(),
        Wake::Due,
        "a deadline that came"
    );
    stop.set();
    assert_eq!(
        wait(
            &mut seen,
            &mut network,
            &[&stop],
            Instant::now() + MUST_HAPPEN
        )
        .unwrap(),
        Wake::Stopped
    );
    assert!(
        !wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "a set stop"
    );
}

/// The wake is the latest deadline within the coalescing reach (the old tick) of the earliest, so
/// duties due that close share it, and a shorter tick reaches less; none on a clock is none; the
/// old pace and a settling aterm.toml are never put later by it. The pacer passes a future
/// deadline through, lets one past deadline run at once, and puts a second running past one a
/// pace ahead. Memory only.
/// NEGATIVE CONTROLS: a deadline that stays past is not handed back as now a second time (a
/// spin); the deadlines that keep the pace, with a timed duty in the pace's place, share the
/// later wake, so the pace's is kept because it is the pace.
#[test]
fn duties_share_a_wake_and_a_stuck_deadline_costs_the_pace() {
    let now = Instant::now();
    let s = Duration::from_secs;
    let reach = s(2);
    assert_eq!(
        Duties::default().earliest(reach),
        None,
        "nothing on a clock"
    );
    let cases = [
        (
            Duties {
                backstop: Some(now + s(180)),
                ..Duties::default()
            },
            now + s(180),
            "the backstop alone",
        ),
        (
            Duties {
                heartbeat: Some(now + s(180)),
                facts: Some(now + s(181)),
                backstop: Some(now + s(180)),
                ..Duties::default()
            },
            now + s(181),
            "a facts push a second after the heartbeat shares its wake",
        ),
        (
            Duties {
                heartbeat: Some(now + s(10)),
                census: Some(now + s(13)),
                ..Duties::default()
            },
            now + s(10),
            "a census three seconds later wakes apart",
        ),
        (
            Duties {
                maintenance: Some(now),
                paced: Some(now + s(2)),
                backstop: Some(now + s(180)),
                ..Duties::default()
            },
            now + s(2),
            "the pace is met at the edge of the reach",
        ),
        (
            Duties {
                settle: Some(now + Duration::from_millis(600)),
                advert: Some(now + s(60)),
                ..Duties::default()
            },
            now + Duration::from_millis(600),
            "a settling aterm.toml is judged at its settle",
        ),
        (
            Duties {
                checkouts: Some(now + s(30)),
                census: Some(now + s(31)),
                backstop: Some(now + s(180)),
                ..Duties::default()
            },
            now + s(31),
            "the checkouts read shares the census's wake",
        ),
        (
            Duties {
                routes: Some(now + s(30)),
                advert: Some(now + s(60)),
                backstop: Some(now + s(180)),
                ..Duties::default()
            },
            now + s(30),
            "the routes' refresh wakes on its own period",
        ),
        (
            Duties {
                paced: Some(now + s(2)),
                heartbeat: Some(now + Duration::from_millis(3_900)),
                ..Duties::default()
            },
            now + s(2),
            "the old pace is kept exactly: a duty within its reach does not put it later",
        ),
        (
            Duties {
                settle: Some(now + Duration::from_millis(600)),
                facts: Some(now + Duration::from_millis(2_500)),
                ..Duties::default()
            },
            now + Duration::from_millis(600),
            "a settling aterm.toml is judged at its settle, a duty within its reach or not",
        ),
        (
            Duties {
                heartbeat: Some(now + s(1)),
                paced: Some(now + s(2)),
                census: Some(now + Duration::from_millis(2_900)),
                ..Duties::default()
            },
            now + s(2),
            "a duty before the pace shares the pace's wake, not a later one's",
        ),
    ];
    for (duties, wake, why) in cases {
        assert_eq!(duties.earliest(reach), Some(wake), "{why}");
    }
    assert_eq!(
        Duties {
            facts: Some(now + s(2)),
            heartbeat: Some(now + Duration::from_millis(3_900)),
            ..Duties::default()
        }
        .earliest(reach),
        Some(now + Duration::from_millis(3_900)),
        "CONTROL: the same deadlines with a duty in the pace's place share the later wake"
    );
    let short = Duties {
        heartbeat: Some(now + s(1)),
        facts: Some(now + s(2)),
        ..Duties::default()
    };
    assert_eq!(
        short.earliest(Duration::from_millis(20)),
        Some(now + s(1)),
        "a 20 ms tick shares no wake a second apart"
    );

    let pace = s(2);
    let mut pacer = Pacer::default();
    assert_eq!(
        pacer.next(now, now + s(5), pace),
        now + s(5),
        "a future deadline"
    );
    assert_eq!(
        pacer.next(now, now, pace),
        now,
        "a past deadline runs at once"
    );
    assert_eq!(
        pacer.next(now, now, pace),
        now + pace,
        "CONTROL: one still past is a pace ahead, not now again"
    );
    assert_eq!(
        pacer.next(now, now + s(1), pace),
        now + s(1),
        "a moved one resets"
    );
    assert_eq!(pacer.next(now, now, pace), now, "and runs at once again");
}

/// A heartbeat round a pass owed and could not start is asked again an interval on, never at the
/// past instant the clock still names; a round that started (none while it is out), one not yet
/// owed, and one a pass did not owe keep the clock's answer. Memory only.
/// NEGATIVE CONTROL: the clock's own answer for the refused round is at or before the pass, a
/// deadline every wake would meet (the pacer then costs the old tick at each).
#[test]
fn a_refused_heartbeat_round_waits_an_interval_not_the_old_tick() {
    let now = Instant::now();
    let every = Duration::from_secs(180);
    let past = now - Duration::from_secs(1);
    assert!(past <= now, "CONTROL: the clock names a past instant");
    assert_eq!(
        heartbeat_after(Some(past), true, now, every),
        Some(now + every),
        "refused: an interval on"
    );
    assert_eq!(
        heartbeat_after(None, true, now, every),
        None,
        "started: none while it is out"
    );
    let later = now + every;
    assert_eq!(heartbeat_after(Some(later), true, now, every), Some(later));
    assert_eq!(
        heartbeat_after(Some(past), false, now, every),
        Some(past),
        "a move made due after the pass asked is the clock's own"
    );
}

/// A `[swarm] enabled` that does not read holds once it has not read for the settle cap, at once
/// with no event to wake the judgment again; one that reads off holds at once, on or no file at
/// all holds nothing, and a reading ends an episode. Memory only.
/// NEGATIVE CONTROL: within the cap it does not hold, and its settle names the cap's end.
#[test]
fn an_unreadable_switch_holds_once_the_write_settles() {
    let now = Instant::now();
    let cap = aterm_uds::file_watch::SETTLE_CAP;
    let unread = || Some(Err::<bool, ()>(()));
    let mut unreadable = Unreadable::default();
    assert!(
        !unreadable.holds(unread(), true, now),
        "CONTROL: not within the cap"
    );
    assert_eq!(
        unreadable.settle(),
        Some(now + cap),
        "judged again at the cap"
    );
    assert!(
        !unreadable.holds(unread(), true, now + cap - Duration::from_millis(1)),
        "CONTROL: not before it"
    );
    assert!(
        unreadable.holds(unread(), true, now + cap),
        "left unreadable: fail closed"
    );
    assert!(
        !unreadable.holds(Some(Ok::<bool, ()>(true)), true, now + cap),
        "it reads on again"
    );
    assert_eq!(unreadable.settle(), None, "the episode ended");
    assert!(
        unreadable.holds(Some(Ok::<bool, ()>(false)), true, now),
        "off holds at once"
    );
    assert!(
        !unreadable.holds(None::<Result<bool, ()>>, true, now),
        "no aterm.toml: no switch"
    );
    assert!(
        Unreadable::default().holds(unread(), false, now),
        "with no event to wake it, at once as before"
    );
}

/// What every fleet listener watches: the fleet's state files, aterm.toml, and every vault file a
/// judgment re-reads, the standing and acknowledged history included, and the records' data
/// wherever a layout puts it. Paths only, of a scratch home.
/// NEGATIVE CONTROL: the check names a file left out of the set.
#[test]
fn the_watch_set_holds_every_input_a_judgment_reads() {
    use aterm_net::fleet_member::{
        ISSUER_BOOK_FILE, NODE_FILE, NODE_KEY_FILE, OFF_FILE, PEERS_FILE, VAULT_FILE,
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let config = root.path().join("aterm.toml");
    let vault = aterm_secrets::client::Paths::locate_for_home(root.path());
    let mut required: Vec<PathBuf> = [
        aterm_net::fleet::KEY_FILE,
        OFF_FILE,
        NODE_FILE,
        NODE_KEY_FILE,
        VAULT_FILE,
        PEERS_FILE,
        ISSUER_BOOK_FILE,
    ]
    .into_iter()
    .map(|name| state.join(name))
    .collect();
    required.push(config.clone());
    required.extend(vault.standing_files());
    required.extend(vault.acknowledged_history_files());
    required.push(vault.dir.join("vault"));
    required.push(
        vault
            .records_data_file()
            .expect("a plain records entry resolves"),
    );
    // A layout's data file sits inside the entry: a move into a layout renames nothing watched.
    required.push(vault.records_file().join("current"));
    let missing = |set: &[PathBuf]| -> Vec<PathBuf> {
        required
            .iter()
            .filter(|file| !set.contains(file))
            .cloned()
            .collect()
    };
    let set = fleet_files(&state, Some(&config), Some(&vault));
    assert_eq!(
        missing(&set),
        Vec::<PathBuf>::new(),
        "every input is watched"
    );
    let without_key: Vec<PathBuf> = set
        .iter()
        .filter(|file| **file != state.join(aterm_net::fleet::KEY_FILE))
        .cloned()
        .collect();
    assert_eq!(
        missing(&without_key),
        vec![state.join(aterm_net::fleet::KEY_FILE)],
        "CONTROL: the check names the file left out"
    );
}

/// A watched file's change rings its registration's bell, an unwatched sibling's does not, and a
/// dropped registration rings no more. Real platform file events, in a scratch folder.
/// NEGATIVE CONTROL: the generation is counted exactly after later replaces of the watched file,
/// each a rename that moves its stamp once (so each is one tell, however the watch's settling
/// falls), so a ring from the sibling would show as one too many (watch the sibling too and the
/// count is one higher).
#[test]
fn a_watched_file_rings_its_bell_and_a_sibling_does_not() {
    let (inputs, error) = Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify));
    assert!(error.is_none(), "the platform watch starts: {error:?}");
    let inputs = Arc::new(inputs);
    assert!(inputs.watching());
    let root = aterm_tempfile::tempdir().unwrap();
    let watched_file = root.path().join("fleet.key");
    let sibling = root.path().join("unrelated");
    std::fs::write(&watched_file, b"one").unwrap();
    let watched = inputs.register();
    let mut seen = Seen::new(watched.bell());
    let stop = Stop::new();
    watched.file(watched_file.clone());
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "its first look rings"
    );
    let first = watched.bell().generation();
    std::fs::write(&sibling, b"noise").unwrap();
    replace(&watched_file, b"two");
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "the replace rings"
    );
    replace(&watched_file, b"three");
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "again"
    );
    assert_eq!(
        watched.bell().generation(),
        first + 2,
        "two replaces of the watched file, and nothing from its sibling"
    );
    let bell = Arc::clone(watched.bell());
    drop(watched);
    assert!(
        inputs.routes.lock_or_recover().is_empty(),
        "a dropped registration routes nothing"
    );
    let _ = bell;
}

/// A NODE'S RELAY FOLDER RINGS FOR ITS RELAYS ALONE: on macOS the relay folder is the state
/// folder, where every aterm process writes its census, vitals, journals and control sockets, so
/// the node's watch of it rings only where a window's relay socket is made, removed or replaced.
/// Real platform file events, in a scratch folder.
/// NEGATIVE CONTROLS: each write beside the relays is told (the watch's count of its tells moves)
/// before the generation is judged unmoved, so the tell happened and rang nothing, not a tell
/// still to come; the generation then moves by exactly one per relay change; and the same folder
/// kept whole rings for the write beside them alone. Make the listing always moved and the first
/// judgment fails.
#[test]
fn a_relay_folder_rings_for_its_relays_alone() {
    let (inputs, error) = Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify));
    assert!(error.is_none(), "the platform watch starts: {error:?}");
    let inputs = Arc::new(inputs);
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let stop = Stop::new();
    let watched = inputs.register();
    let mut seen = Seen::new(watched.bell());
    watched.entries(state.clone(), is_relay);
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "its first look rings"
    );
    let first = watched.bell().generation();
    // A write beside the relays, told and judged: it rang nothing.
    let beside = |name: &str| {
        let told = watched.tells();
        replace(&state.join(name), b"{}");
        aterm_spec::wait::until_within(MUST_HAPPEN, "the write beside is told", || {
            (watched.tells() > told).then_some(())
        });
    };
    let relay = state.join("wf-4242.sock");
    beside("swarm-census.json");
    assert_eq!(
        watched.bell().generation(),
        first,
        "a census write beside the relays rings nothing"
    );
    std::fs::write(&relay, b"").unwrap();
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "a relay made rings"
    );
    assert_eq!(watched.bell().generation(), first + 1, "once");
    beside("vitals-free.json");
    assert_eq!(
        watched.bell().generation(),
        first + 1,
        "a vitals write beside the relays rings nothing"
    );
    std::fs::remove_file(&relay).unwrap();
    assert!(
        wait_rung(&mut seen, &stop, Instant::now() + MUST_HAPPEN),
        "a relay removed rings"
    );
    assert_eq!(watched.bell().generation(), first + 2, "once");

    let whole = inputs.register();
    let mut whole_seen = Seen::new(whole.bell());
    whole.entries(state.clone(), |_| true);
    assert!(
        wait_rung(&mut whole_seen, &stop, Instant::now() + MUST_HAPPEN),
        "its first look rings"
    );
    replace(&state.join("swarm-census.json"), b"{\"again\":1}");
    assert!(
        wait_rung(&mut whole_seen, &stop, Instant::now() + MUST_HAPPEN),
        "CONTROL: the folder kept whole rings for the census's replace"
    );
}

/// A watch that cannot start leaves every caller on its old tick: nothing is watched (the vault's
/// `state.toml` and its standing filter included), an unreadable switch is acted on at once, and
/// a node's inputs say they are not watched. Memory only.
/// NEGATIVE CONTROL: the same set over a watch that starts says it watches.
#[test]
fn a_watch_that_cannot_start_keeps_the_old_tick() {
    let (failed, error) = Inputs::start(|_| Err(std::io::Error::other("no file events")));
    assert!(error.is_some(), "its error is handed back to be said");
    let failed = Arc::new(failed);
    assert!(!failed.watching());
    let root = aterm_tempfile::tempdir().unwrap();
    let vault = aterm_secrets::client::Paths::locate_for_home(root.path());
    let node = NodeInputs::start(
        &failed,
        root.path(),
        None,
        Some(&vault),
        Vec::new(),
        relays(root.path()),
    );
    assert!(!node.watching(), "the node keeps its tick");
    assert!(
        failed.routes.lock_or_recover().is_empty(),
        "nothing is watched"
    );
    let (running, _) = Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify));
    let running = Arc::new(running);
    let node = NodeInputs::start(
        &running,
        root.path(),
        None,
        None,
        Vec::new(),
        relays(root.path()),
    );
    assert!(node.watching(), "CONTROL: a watch that starts watches");
    assert!(
        !running.routes.lock_or_recover().is_empty(),
        "CONTROL: and routes its files"
    );
}

/// The node's upkeep woken by a worker it keeps (the keeper's end) rings every node job's inputs
/// in this process, whether or not their file watch runs, and a dropped job's inputs are let go.
/// Memory only.
/// NEGATIVE CONTROL: before the wake nothing is pending, and the dropped job's inputs are not
/// held.
#[test]
fn a_maintenance_wake_rings_every_node_job() {
    let (failed, _) = Inputs::start(|_| Err(std::io::Error::other("no file events")));
    let failed = Arc::new(failed);
    let root = aterm_tempfile::tempdir().unwrap();
    let node = NodeInputs::start(
        &failed,
        root.path(),
        None,
        None,
        Vec::new(),
        relays(root.path()),
    );
    let mut seen = Seen::new(node.bell());
    let gone = NodeInputs::start(
        &failed,
        root.path(),
        None,
        None,
        Vec::new(),
        relays(root.path()),
    );
    let gone_job = Arc::downgrade(&gone);
    drop(gone);
    assert!(!seen.take(), "CONTROL: nothing pending before the wake");
    wake_node_maintenance();
    assert!(seen.take(), "the wake rings the job's inputs");
    assert!(
        gone_job.upgrade().is_none(),
        "CONTROL: the dropped job's inputs are not held"
    );
    assert!(
        NODES
            .lock_or_recover()
            .iter()
            .all(|(_, job)| !job.ptr_eq(&gone_job)),
        "the wake let the dropped job's inputs go"
    );
}

/// A node watch whose file watch did not start keeps the old tick exactly: a ring wakes no pass,
/// a move refreshes admission as it did, and the tick's pass comes with nothing rung. Memory and
/// a private network signal only.
/// NEGATIVE CONTROL: over a watch that runs, the same ring runs a pass at once, ahead of the move.
#[test]
fn a_node_watch_with_no_file_events_keeps_the_old_tick() {
    use crate::network_moved::{Moved, Woke};
    let stop = Stop::new();
    let root = aterm_tempfile::tempdir().unwrap();
    let start = |watching: bool| {
        let (inputs, _) = if watching {
            Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify))
        } else {
            Inputs::start(|_| Err(std::io::Error::other("no file events")))
        };
        NodeInputs::start(
            &Arc::new(inputs),
            root.path(),
            None,
            None,
            Vec::new(),
            relays(root.path()),
        )
    };
    for watching in [false, true] {
        let node = start(watching);
        assert_eq!(node.watching(), watching);
        let moved = Arc::new(Moved::new());
        let mut network = moved.watch_at_once();
        let mut pace = Pace::node(&node, Seen::new(node.bell()), Duration::from_secs(3600));
        node.bell().ring();
        moved.tell_at(Woke::Route, Instant::now());
        let pass = pace.wait(&mut network, &[&stop]).expect("not stopped");
        if watching {
            assert_eq!(
                pass,
                Pass {
                    moved: None,
                    run: true
                },
                "CONTROL: a ring runs a pass at once"
            );
        } else {
            assert_eq!(
                pass,
                Pass {
                    moved: Some(Woke::Route),
                    run: false
                },
                "a ring runs no pass; the move refreshes admission"
            );
        }
    }
    let node = start(false);
    let mut network = Arc::new(Moved::new()).watch_at_once();
    let mut pace = Pace::node(&node, Seen::new(node.bell()), Duration::from_millis(1));
    assert_eq!(
        pace.wait(&mut network, &[&stop]),
        Some(Pass {
            moved: None,
            run: true
        }),
        "the tick's pass"
    );
}

/// A waiter on a held port lock rings every bell armed on it once the holder lets go, one thread
/// per lock however many bells. Files only, in a scratch state folder.
/// NEGATIVE CONTROL: the probe says held before the release, so the ring is the release's.
#[test]
fn a_port_lock_released_rings_each_bell_waiting_on_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let held = aterm_agent::node_service::NodeJobLock::try_acquire_port(&state)
        .unwrap()
        .expect("a free lock");
    assert!(
        aterm_agent::node_service::node_port_holds_lock(&state),
        "CONTROL: held"
    );
    let first = Arc::new(Bell::default());
    let second = Arc::new(Bell::default());
    let mut first_seen = Seen::new(&first);
    let mut second_seen = Seen::new(&second);
    let name = aterm_agent::node_service::NODE_PORT_LOCK;
    assert!(ring_on_release(&state, name, &first));
    assert!(ring_on_release(&state, name, &second));
    assert_eq!(
        PORT_WAITERS
            .lock_or_recover()
            .iter()
            .filter(|(at, _)| *at == state.join(name))
            .count(),
        1,
        "one waiter for the lock"
    );
    drop(held);
    let stop = Stop::new();
    assert!(wait_rung(
        &mut first_seen,
        &stop,
        Instant::now() + MUST_HAPPEN
    ));
    assert!(wait_rung(
        &mut second_seen,
        &stop,
        Instant::now() + MUST_HAPPEN
    ));
}

/// Node inputs on `state` over a file watch that runs (`watching`) or did not start, its relays
/// in `relay_dir`.
fn node_inputs(state: &Path, relay_dir: &Path, watching: bool) -> Arc<NodeInputs> {
    let (inputs, _) = if watching {
        Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify))
    } else {
        Inputs::start(|_| Err(std::io::Error::other("no file events")))
    };
    NodeInputs::start(
        &Arc::new(inputs),
        state,
        None,
        None,
        Vec::new(),
        relays(relay_dir),
    )
}

/// A NODE'S REST WITH NO FILE EVENTS KEEPS THE OLD TICK: where the file watch did not start, the
/// outer loop's rest sleeps its tick on the job's stop whatever rang, and ends at that stop.
/// Memory and a scratch folder only.
/// NEGATIVE CONTROL: over a watch that runs, the same ring ends the rest at once, with no sleep on
/// the stop.
#[test]
fn a_node_rest_with_no_file_events_keeps_the_old_tick() {
    let root = aterm_tempfile::tempdir().unwrap();
    let relays = root.path().join("relays");
    std::fs::create_dir(&relays).unwrap();
    let rest = |watching: bool, stop: &Stop| {
        let node = node_inputs(root.path(), &relays, watching);
        let mut idle = NodeIdle::new(&node, root.path(), Duration::from_secs(3600));
        let mut seen = Seen::new(node.bell());
        node.bell().ring();
        idle.wait(stop, &mut seen, Idle::Inputs);
    };

    let stop = Stop::new();
    rest(true, &stop);
    assert_eq!(stop.entered(), 0, "CONTROL: the ring ended the rest");

    let stop = Arc::new(Stop::new());
    let stopper = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            aterm_spec::wait::until_within(MUST_HAPPEN, "the rest sleeps its tick", || {
                (stop.entered() > 0).then_some(())
            });
            stop.set();
        })
    };
    rest(false, &stop);
    stopper.join().expect("the rest slept its tick on the stop");
    assert!(stop.is_set(), "the rest ended at the stop, not at the ring");
}

/// A PORT HELD BY NO WINDOW BACKS OFF; A WINDOW'S IS WAITED ON: a bind that met the port in use
/// while no window holds `window-port.lock` tries again after the old tick, then twice as long
/// each time up to [`FOREIGN_MOST`], and a rest for another reason or a ring starts it from the
/// tick again. While a window holds that lock, the rest waits for its release with no clock.
/// Files only, in a scratch state folder.
/// NEGATIVE CONTROL: the waiter is armed while the window holds the lock and gone once it lets
/// go, so the wait is the release's.
#[test]
fn a_port_held_by_no_window_backs_off_and_a_windows_release_is_waited_on() {
    use aterm_agent::node_service::{NodeJobLock, WINDOW_PORT_LOCK};
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let relays = root.path().join("relays");
    std::fs::create_dir(&relays).unwrap();
    let node = node_inputs(&state, &relays, true);
    let tick = Duration::from_secs(2);
    let mut idle = NodeIdle::new(&node, &state, tick);
    let now = Instant::now();
    let retry = |idle: &mut NodeIdle| {
        idle.arm(now, Idle::PortInUse)
            .and_then(|duties| duties.retry)
            .map(|at| at - now)
    };
    let backoff: Vec<Option<Duration>> = (0..7).map(|_| retry(&mut idle)).collect();
    assert_eq!(
        backoff,
        [2, 4, 8, 16, 32, 60, 60].map(|secs| Some(Duration::from_secs(secs)))
    );
    idle.arm(now, Idle::Inputs);
    assert_eq!(retry(&mut idle), Some(tick), "another rest starts it again");
    retry(&mut idle); // backed off once more, which the ring then resets
    let mut seen = Seen::new(node.bell());
    node.bell().ring();
    idle.wait(&Stop::new(), &mut seen, Idle::PortInUse);
    assert_eq!(retry(&mut idle), Some(tick), "a ring starts it again");

    let window = NodeJobLock::try_acquire_window_port(&state)
        .unwrap()
        .expect("a free lock");
    let duties = idle.arm(now, Idle::PortInUse).expect("duties");
    assert_eq!(
        (duties.retry, duties.paced),
        (None, None),
        "a window's release is an event"
    );
    assert!(waiting_on(&state, WINDOW_PORT_LOCK), "CONTROL: armed");
    drop(window);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the waiter hears the release", || {
        (!waiting_on(&state, WINDOW_PORT_LOCK)).then_some(())
    });
}

/// A tick no test outlives: a wake at it would come long after any test's hang detector.
const NO_TICK: Duration = Duration::from_secs(3600);

/// Window inputs on `state` over a file watch that runs (`watching`) or did not start, once the
/// watch told each of their files (whose rings come before they are noted), so a view taken then
/// counts only what a test changes.
fn window_inputs(state: &Path, watching: bool) -> Arc<WindowInputs> {
    let (inputs, _) = if watching {
        Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify))
    } else {
        Inputs::start(|_| Err(std::io::Error::other("no file events")))
    };
    let window = WindowInputs::start(&Arc::new(inputs), Some(state), None, None, Vec::new());
    assert_eq!(window.watching(), watching);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the window's first looks are told", || {
        window.first_looks_told().then_some(())
    });
    window
}

/// A NODE TAKING THE PORT RINGS A SERVING WINDOW: the node job's take of `node-port.lock` writes
/// its pid, which the window's watch hears, so a window that has the port bound hands it over at
/// that event, not at a clock. Real platform file events, in a scratch state folder.
/// NEGATIVE CONTROL: a window's probe of the lock moves nothing its watch hears, so probes ring
/// nothing. Take `node-port.lock` out of [`WindowInputs::start`]'s set and the take's ring never
/// comes (the hang detector fails it).
#[test]
fn a_node_taking_the_port_rings_a_serving_window() {
    use aterm_agent::node_service::{NODE_PORT_LOCK, NodeJobLock, node_port_holds_lock};
    use aterm_uds::file_watch::FileStamp;
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    // A take made and let go before the window watches: the file is there, and the take the
    // test makes moves its stamp.
    drop(
        NodeJobLock::try_acquire_port(&state)
            .unwrap()
            .expect("a free lock"),
    );
    let window = window_inputs(&state, true);
    let lock = state.join(NODE_PORT_LOCK);
    let before = FileStamp::of(&lock);
    assert!(before.is_some(), "the lock file is there");
    assert!(!node_port_holds_lock(&state), "no node holds the port");
    assert_eq!(
        FileStamp::of(&lock),
        before,
        "CONTROL: a probe moves nothing a watch hears"
    );
    let mut seen = Seen::new(window.bell());
    let held = NodeJobLock::try_acquire_port(&state)
        .unwrap()
        .expect("the node's take");
    assert!(
        wait_rung(&mut seen, &Stop::new(), Instant::now() + MUST_HAPPEN),
        "the take rings the window"
    );
    drop(held);
}

/// A WINDOW THAT LEAVES THE PORT TO THE NODE WAKES AT THE NODE'S RELEASE: its rest waits on
/// `node-port.lock`, and the holder letting go (the job's end, or its crash, whose lock the kernel
/// lets go) wakes it, with no clock: its old tick and its backstop are an hour. Files only, in a
/// scratch state folder; the window's rest runs in this process.
/// NEGATIVE CONTROL: while the holder holds, the rest wakes only at the ring the test makes, once,
/// and rests on.
#[test]
fn a_window_left_the_port_by_the_node_wakes_at_its_release() {
    use aterm_agent::node_service::{NODE_PORT_LOCK, NodeJobLock};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let held = NodeJobLock::try_acquire_port(&state)
        .unwrap()
        .expect("the node's take");
    let window = window_inputs(&state, true);
    let woke = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicBool::new(true));
    let rester = {
        let (window, state) = (Arc::clone(&window), state.clone());
        let (woke, current) = (Arc::clone(&woke), Arc::clone(&current));
        std::thread::spawn(move || {
            let network = Arc::new(crate::network_moved::Moved::new()).watch_at_once();
            let mut rest = WindowRest::new(&window, Some(&state), (NO_TICK, NO_TICK), network);
            let mut seen = Seen::new(window.bell());
            while current.load(Ordering::Acquire) {
                rest.wait(&mut seen, Rest::NodeHolds, &|| {
                    current.load(Ordering::Acquire)
                });
                woke.fetch_add(1, Ordering::AcqRel);
            }
        })
    };
    aterm_spec::wait::until_within(MUST_HAPPEN, "the rest waits on the node's lock", || {
        (rests(&state) >= 1 && waiting_on(&state, NODE_PORT_LOCK)).then_some(())
    });
    window.bell().ring();
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the ring's wake, and the rest after it",
        || (rests(&state) >= 2).then_some(()),
    );
    assert_eq!(
        woke.load(Ordering::Acquire),
        1,
        "CONTROL: one wake while the node holds, the test's ring"
    );
    drop(held);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the release wakes the rest", || {
        (woke.load(Ordering::Acquire) >= 2).then_some(())
    });
    current.store(false, Ordering::Release);
    window.bell().ring();
    rester.join().expect("the rest ends");
}

/// A WINDOW'S REST WITH NO FILE EVENTS KEEPS THE OLD WAIT: where the watch did not start, a ring
/// after which the listener stands unchanged (a reload that moves nothing it waits on) rests on,
/// as the old condition wait did; no port waiter is armed; and a ring after a change ends it.
/// Memory and scratch folders only.
/// NEGATIVE CONTROL: over a watch that runs, the same ring with nothing changed ends the rest at
/// once. Off the watch, a second ring that changed nothing is judged by the same rest, so the
/// first did not end it (counted, not timed).
#[test]
fn a_window_rest_with_no_file_events_keeps_the_old_wait() {
    use aterm_agent::node_service::{NODE_PORT_LOCK, NodeJobLock};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let root = aterm_tempfile::tempdir().unwrap();
    let network = || Arc::new(crate::network_moved::Moved::new()).watch_at_once();

    let watched_state = root.path().join("watched");
    let window = window_inputs(&watched_state, true);
    let mut rest = WindowRest::new(&window, Some(&watched_state), (NO_TICK, NO_TICK), network());
    let mut seen = Seen::new(window.bell());
    window.bell().ring();
    rest.wait(&mut seen, Rest::Inputs, &|| true);
    assert!(
        !seen.take(),
        "CONTROL: the ring ended the rest, and was taken"
    );

    let state = root.path().join("state");
    let held = NodeJobLock::try_acquire_port(&state)
        .unwrap()
        .expect("the node's take");
    let window = window_inputs(&state, false);
    let judged = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(AtomicBool::new(false));
    let rester = {
        let (window, state) = (Arc::clone(&window), state.clone());
        let (judged, changed) = (Arc::clone(&judged), Arc::clone(&changed));
        std::thread::spawn(move || {
            let mut rest = WindowRest::new(&window, Some(&state), (NO_TICK, NO_TICK), network());
            let mut seen = Seen::new(window.bell());
            rest.wait(&mut seen, Rest::NodeHolds, &|| {
                judged.fetch_add(1, Ordering::AcqRel);
                !changed.load(Ordering::Acquire)
            });
        })
    };
    aterm_spec::wait::until_within(MUST_HAPPEN, "the rest judges the listener", || {
        (judged.load(Ordering::Acquire) >= 1).then_some(())
    });
    window.bell().ring();
    aterm_spec::wait::until_within(MUST_HAPPEN, "the rest judges the ring", || {
        (judged.load(Ordering::Acquire) >= 2).then_some(())
    });
    window.bell().ring();
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "a ring that changed nothing ends no rest off the watch: the same rest judges the next",
        || (judged.load(Ordering::Acquire) >= 3).then_some(()),
    );
    assert!(
        !waiting_on(&state, NODE_PORT_LOCK),
        "no port waiter off the watch"
    );
    changed.store(true, Ordering::Release);
    window.bell().ring();
    rester.join().expect("the change ends the rest");
    drop(held);
}

/// A REFUSED BIND BACKS OFF, AND WHAT CAN FREE IT STARTS IT AGAIN: a bind refused (the port in
/// use, the address not this machine's) is tried again after the old tick, then twice as long
/// each time up to [`FOREIGN_MOST`]; a rest for another reason, a ring and a network move each
/// start it from the tick again; and while another window holds `window-port.lock` its release is
/// waited on beside the backoff. Files only, in a scratch state folder, with a private network
/// signal.
/// NEGATIVE CONTROL: the backoff grows again after each reset, so each reset is the reset's; no
/// waiter is armed while no window holds the port.
#[test]
fn a_refused_bind_backs_off_and_what_can_free_it_starts_it_again() {
    use crate::network_moved::{Moved, Woke};
    use aterm_agent::node_service::{NodeJobLock, WINDOW_PORT_LOCK};
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let window = window_inputs(&state, true);
    let moved = Arc::new(Moved::new());
    let tick = Duration::from_secs(2);
    let mut rest = WindowRest::new(
        &window,
        Some(&state),
        (tick, NO_TICK),
        moved.watch_at_once(),
    );
    let now = Instant::now();
    let retry = |rest: &mut WindowRest| rest.arm(now, Rest::Unbound).1.map(|at| at - now);
    let backoff: Vec<Option<Duration>> = (0..7).map(|_| retry(&mut rest)).collect();
    assert_eq!(
        backoff,
        [2, 4, 8, 16, 32, 60, 60].map(|secs| Some(Duration::from_secs(secs)))
    );
    rest.arm(now, Rest::Inputs);
    assert_eq!(retry(&mut rest), Some(tick), "another rest starts it again");
    assert_eq!(retry(&mut rest), Some(tick * 2), "CONTROL: it grows again");

    let mut seen = Seen::new(window.bell());
    window.bell().ring();
    rest.wait(&mut seen, Rest::Unbound, &|| true);
    assert_eq!(retry(&mut rest), Some(tick), "a ring starts it again");
    assert_eq!(retry(&mut rest), Some(tick * 2), "CONTROL: it grows again");

    moved.tell_at(Woke::Route, Instant::now());
    rest.wait(&mut seen, Rest::Unbound, &|| true);
    assert_eq!(
        retry(&mut rest),
        Some(tick),
        "a network move starts it again"
    );
    assert!(
        !waiting_on(&state, WINDOW_PORT_LOCK),
        "CONTROL: no window holds the port"
    );

    let other = NodeJobLock::try_acquire_window_port(&state)
        .unwrap()
        .expect("another window's custody");
    let (armed, again) = rest.arm(now, Rest::Unbound);
    assert!(
        armed && again.is_some(),
        "the backoff stands beside the release"
    );
    assert!(
        waiting_on(&state, WINDOW_PORT_LOCK),
        "another window's release is waited on"
    );
    drop(other);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the waiter hears the release", || {
        (!waiting_on(&state, WINDOW_PORT_LOCK)).then_some(())
    });
}

/// A WINDOW'S SERVING WATCH WITH NO FILE EVENTS KEEPS THE OLD TICK: where the watch did not start,
/// a change of `[fleet]` (a ring) runs a pass at once, a move wakes nothing (the heartbeat clock's
/// own watch takes it at the next pass), and the tick's pass comes with nothing rung; a stop ends
/// it. Memory and a private network signal only.
/// NEGATIVE CONTROL: over a watch that runs, the move wakes the watch, which runs no pass for it.
#[test]
fn a_window_watch_with_no_file_events_keeps_the_old_tick() {
    use crate::network_moved::{Moved, Woke};
    let stop = Stop::new();
    let root = aterm_tempfile::tempdir().unwrap();
    for watching in [false, true] {
        let window = window_inputs(&root.path().join(format!("{watching}")), watching);
        let moved = Arc::new(Moved::new());
        let mut network = moved.watch_at_once();
        let mut pace = Pace::window(&window, Seen::new(window.bell()), NO_TICK, NO_TICK);
        moved.tell_at(Woke::Route, Instant::now());
        if watching {
            assert_eq!(
                pace.wait(&mut network, &[&stop]),
                Some(Pass {
                    moved: Some(Woke::Route),
                    run: false
                }),
                "CONTROL: over the watch a move wakes it, and runs no pass"
            );
        } else {
            window.bell().ring();
            assert_eq!(
                pace.wait(&mut network, &[&stop]),
                Some(Pass {
                    moved: None,
                    run: true
                }),
                "a change of [fleet] runs a pass at once"
            );
            assert_eq!(
                network.take(),
                Some(Woke::Route),
                "the move woke nothing: it is still unheard"
            );
        }
    }
    let window = window_inputs(&root.path().join("ticked"), false);
    let mut network = Arc::new(Moved::new()).watch_at_once();
    let mut pace = Pace::window(
        &window,
        Seen::new(window.bell()),
        Duration::from_millis(1),
        NO_TICK,
    );
    assert_eq!(
        pace.wait(&mut network, &[&stop]),
        Some(Pass {
            moved: None,
            run: true
        }),
        "the tick's pass"
    );
    stop.set();
    assert_eq!(pace.wait(&mut network, &[&stop]), None, "the stop ends it");
}

/// A CHANGE OF THE WINDOW'S `[fleet]` RINGS ITS LISTENER: [`ring_window`] rings this process's
/// window inputs once they started, so every wait of its listener judges again at once. This
/// process's own file watch, over a scratch state folder.
/// NEGATIVE CONTROL: before the ring nothing is pending.
#[test]
fn a_change_of_fleet_rings_the_windows_inputs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let window = window(|inputs| WindowInputs::start(inputs, Some(&state), None, None, Vec::new()));
    aterm_spec::wait::until_within(MUST_HAPPEN, "the window's first looks are told", || {
        window.first_looks_told().then_some(())
    });
    let mut seen = Seen::new(window.bell());
    assert!(!seen.take(), "CONTROL: nothing pending");
    ring_window();
    assert!(seen.take(), "the change rings the window's inputs");
}

/// Replace `path`'s contents as [`replace`] does, the file private to this user as the vault's
/// reads require.
fn replace_private(path: &Path, contents: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let next = path.with_extension("next");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&next)
        .unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    drop(file);
    std::fs::rename(&next, path).unwrap();
}

/// A vault's `state.toml` as its subscriber writes it: this Mac's membership, the version it
/// applied and the entries it holds (`held`, `[held]`'s lines), when the vault last heard it, and
/// the receipt of its last refresh round, completed at `receipt_ms`.
fn state_text(membership: &str, held: &str, heard: u64, receipt_ms: u64) -> String {
    format!(
        "# aterm secrets: what this machine holds. Written by aterm; names and digests only.\n\
         membership = \"{membership}\"\nseq = 7\nheard = {heard}\nrequested = 0\n\n[held]\n{held}\
         \n[network_refresh]\nversion = 1\nconfiguration = \"{configuration}\"\nrecipient = \"\"\n\
         completed_at_ms = {receipt_ms}\noutcome = \"succeeded\"\n",
        configuration = "ab".repeat(32),
    )
}

/// One entry of `[held]`.
fn held(name: &str, digest: char) -> String {
    format!("\"{name}\" = \"{}\"\n", String::from(digest).repeat(64))
}

/// A vault folder under `home` that a vault's reads admit.
fn vault_at(home: &Path) -> aterm_secrets::client::Paths {
    use std::os::unix::fs::DirBuilderExt as _;
    let vault = aterm_secrets::client::Paths::locate_for_home(home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&vault.dir)
        .unwrap();
    vault
}

/// A vault's `state.toml` watched by its standing ([`Watched::standing`]) beside the same file
/// watched by its change ([`Watched::file`]), each with its waiter's view.
struct StateWatch {
    state: PathBuf,
    standing: Watched,
    seen: Seen,
    raw: Watched,
    raw_seen: Seen,
    stop: Stop,
}

impl StateWatch {
    /// Change the file by `change`: it rings the standing's bell exactly once, and the raw one.
    /// It returns once the watch counted the tell that rang (a tell rings before it is counted),
    /// so the next [`Self::quiet`] counts from after it and waits for its own rewrite's tell.
    fn rings(&mut self, change: impl FnOnce(&Path), what: &str) {
        let at = self.standing.bell().generation();
        let told = self.standing.tells();
        change(&self.state);
        assert!(
            wait_rung(&mut self.seen, &self.stop, Instant::now() + MUST_HAPPEN),
            "{what} rings"
        );
        assert_eq!(
            self.standing.bell().generation(),
            at + 1,
            "{what} rings once"
        );
        assert!(
            wait_rung(&mut self.raw_seen, &self.stop, Instant::now() + MUST_HAPPEN),
            "and by its change"
        );
        aterm_spec::wait::until_within(MUST_HAPPEN, &format!("{what}: its tell counted"), || {
            (self.standing.tells() > told).then_some(())
        });
    }

    /// Rewrite the file to `text`: the rewrite is told and rings nothing, where the same file
    /// watched by its change rings.
    fn quiet(&mut self, text: &str, what: &str) {
        let at = self.standing.bell().generation();
        let told = self.standing.tells();
        replace_private(&self.state, text);
        aterm_spec::wait::until_within(MUST_HAPPEN, "the rewrite is told", || {
            (self.standing.tells() > told).then_some(())
        });
        assert_eq!(
            self.standing.bell().generation(),
            at,
            "{what} rings nothing"
        );
        assert!(
            wait_rung(&mut self.raw_seen, &self.stop, Instant::now() + MUST_HAPPEN),
            "CONTROL: watched by its change, {what} rings"
        );
    }
}

/// THE VAULT'S STATE RINGS ONLY WHERE ITS STANDING MOVED: a rewrite of `state.toml` that moves
/// only when the vault last heard this Mac or its rounds' receipts (the vault subscriber's
/// heartbeat, every 25-60 s) is told and rings nothing; a membership or entries change rings at
/// its event; the file's first sight and its removal ring; and a file that does not read (a write
/// met mid-way, a mode the vault refuses) rings, as does the tell after it whatever it reads.
/// Real platform file events, in a scratch vault folder.
/// NEGATIVE CONTROLS: each quiet rewrite is told (the watch's count of its tells moves) before the
/// generation is judged unmoved, so the tell happened and rang nothing, not a tell still to come,
/// and the tell of every ring before it is counted first, so the count that moves is the
/// rewrite's (a tell rings before it is counted); the same file watched by its change ([`Watched::file`]) rings for each of them, so each was a
/// change the filter took out; and the generation moves by exactly one per ring the test names.
/// Make [`Standing::moved`] always true and the first quiet judgment fails; make an unread tell
/// ring nothing and the half-written judgment fails.
#[test]
fn the_vault_state_rings_for_its_standing_alone() {
    let (inputs, error) = Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify));
    assert!(error.is_none(), "the platform watch starts: {error:?}");
    let inputs = Arc::new(inputs);
    let root = aterm_tempfile::tempdir().unwrap();
    let vault = vault_at(root.path());
    let standing = inputs.register();
    let raw = inputs.register();
    let mut watch = StateWatch {
        state: state_file(&vault),
        seen: Seen::new(standing.bell()),
        raw_seen: Seen::new(raw.bell()),
        standing,
        raw,
        stop: Stop::new(),
    };
    watch.standing.standing(&vault);
    watch.raw.file(watch.state.clone());
    assert!(
        wait_rung(&mut watch.seen, &watch.stop, Instant::now() + MUST_HAPPEN),
        "its first look rings, the file not there yet"
    );
    assert!(wait_rung(
        &mut watch.raw_seen,
        &watch.stop,
        Instant::now() + MUST_HAPPEN
    ));
    // Counted before the first change counts from it, as each change's own tell is.
    aterm_spec::wait::until_within(MUST_HAPPEN, "its first look counted", || {
        watch.standing.all_told().then_some(())
    });
    let a = held("github", 'a');
    let made = state_text("in", &a, 1_000, 1);
    watch.rings(
        |at| replace_private(at, &made),
        "the file made (its first sight)",
    );
    watch.quiet(&state_text("in", &a, 1_060, 1), "a heard-only rewrite");
    watch.quiet(&state_text("in", &a, 1_060, 2), "a receipt-only rewrite");
    watch.quiet(&state_text("in", &a, 1_120, 3), "heard and a receipt");
    let out = state_text("revoked", &a, 1_120, 3);
    watch.rings(|at| replace_private(at, &out), "membership out");
    let moved = state_text("revoked", &format!("{a}{}", held("npm", 'b')), 1_120, 3);
    watch.rings(|at| replace_private(at, &moved), "an entry moved");
    // Cut inside a quoted string, so what is there does not parse.
    let mid_way = &moved[..moved.find("revoked").expect("its membership") + 3];
    watch.rings(|at| replace_private(at, mid_way), "a write met mid-way");
    watch.rings(
        |at| replace_private(at, &moved),
        "the same standing after a tell that did not read",
    );
    watch.quiet(
        &state_text("revoked", &format!("{a}{}", held("npm", 'b')), 1_180, 4),
        "a heard-only rewrite after them",
    );
    watch.rings(
        |at| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o644)).unwrap();
        },
        "a mode the vault refuses",
    );
    watch.rings(|at| replace_private(at, &moved), "a private file again");
    watch.rings(|at| std::fs::remove_file(at).unwrap(), "the file removed");
}

/// THE NODE'S AND A WINDOW'S INPUTS TAKE THE VAULT'S STATE BY ITS STANDING: their compositions
/// route the `state.toml` of the vault they are given through the standing filter, even where the
/// membership log's files name it too (`aterm_agent::fleet_log::LocalLog::read_files` names the
/// vault's standing files), so the vault subscriber's heartbeat wakes neither, and a membership
/// change wakes both. Real platform file events, in a scratch state folder and vault.
/// NEGATIVE CONTROL: each heartbeat rewrite is told to the composition before its generation is
/// judged unmoved, and the membership change after it rings it exactly once. Watch `state.toml`
/// by its change in [`Watched::files`] and the heartbeat judgment fails.
#[test]
fn the_fleets_inputs_take_the_vault_state_by_its_standing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let relay_dir = root.path().join("relays");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&relay_dir).unwrap();
    let vault = vault_at(&root.path().join("home"));
    let vault_state = state_file(&vault);
    let a = held("github", 'a');
    replace_private(&vault_state, &state_text("in", &a, 1_000, 1));
    let (inputs, error) = Inputs::start(|notify| FileWatch::platform("fleet-inputs-test", notify));
    assert!(error.is_none(), "the platform watch starts: {error:?}");
    let inputs = Arc::new(inputs);
    let node = NodeInputs::start(
        &inputs,
        &state,
        None,
        Some(&vault),
        vault.standing_files(),
        relays(&relay_dir),
    );
    let window = WindowInputs::start(
        &inputs,
        Some(&state),
        None,
        Some(&vault),
        vault.standing_files(),
    );
    let compositions = [("the node", &node.watched), ("a window", &window.watched)];
    aterm_spec::wait::until_within(MUST_HAPPEN, "the first looks are told", || {
        compositions
            .iter()
            .all(|(_, watched)| watched.all_told())
            .then_some(())
    });
    for (who, watched) in compositions {
        assert_eq!(
            watched
                .named
                .lock_or_recover()
                .iter()
                .filter(|path| **path == vault_state)
                .count(),
            1,
            "{who} names the vault's state once"
        );
    }
    let mut seen: Vec<Seen> = compositions
        .iter()
        .map(|(_, watched)| Seen::new(watched.bell()))
        .collect();
    let at: Vec<u64> = compositions
        .iter()
        .map(|(_, watched)| watched.bell().generation())
        .collect();
    for (heard, receipt) in [(1_060, 1), (1_120, 2)] {
        let told: Vec<usize> = compositions.iter().map(|(_, w)| w.tells()).collect();
        replace_private(&vault_state, &state_text("in", &a, heard, receipt));
        aterm_spec::wait::until_within(MUST_HAPPEN, "the heartbeat is told", || {
            compositions
                .iter()
                .zip(&told)
                .all(|((_, watched), told)| watched.tells() > *told)
                .then_some(())
        });
        for ((who, watched), at) in compositions.iter().zip(&at) {
            assert_eq!(
                watched.bell().generation(),
                *at,
                "the heartbeat wakes {who} for nothing"
            );
        }
    }
    replace_private(&vault_state, &state_text("revoked", &a, 1_120, 2));
    let stop = Stop::new();
    for (((who, watched), seen), at) in compositions.iter().zip(&mut seen).zip(&at) {
        assert!(
            wait_rung(seen, &stop, Instant::now() + MUST_HAPPEN),
            "membership out wakes {who}"
        );
        assert_eq!(watched.bell().generation(), at + 1, "once, for {who}");
    }
}
