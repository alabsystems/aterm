// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7c) on the in-process Macs of B7a's
//! tests: each Mac's node service publishes its row on its own `metrics.log`, the relays carry it,
//! and each Mac reads every seat's standing from the rows it holds. Time moves only when a test
//! moves the links' clock and the wall clock together ([`step`]), and a row is known by the wall
//! time it carries, so each wait names the row it waits for; every wait is the one-minute hang
//! detector.

use super::{HANG, Lab, Mac, incarnation, lab_of, lock, seating};
use crate::logs::{MESH_DIR, Standings};
use crate::presence::{
    Build, EVERY, Event, Facts, Held, Presence, PresenceConfig, Refusal, STALE_ROWS_UP, Standing,
};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What one Mac's presence reported.
#[derive(Default)]
struct Heard {
    events: Mutex<Vec<Event>>,
    moved: Condvar,
}

impl Heard {
    fn note(&self, event: &Event) {
        lock(&self.events).push(event.clone());
        self.moved.notify_all();
    }

    fn mark(&self) -> usize {
        lock(&self.events).len()
    }

    /// Wait until `done` holds of the events from `since` on.
    fn until(&self, what: &str, since: usize, done: impl Fn(&[Event]) -> bool) {
        let deadline = Instant::now() + HANG;
        let mut events = lock(&self.events);
        while !done(events.get(since..).unwrap_or_default()) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{what}: not within the hang detector; heard {:#?}",
                events.get(since..)
            );
            events = self
                .moved
                .wait_timeout(events, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Whether a row of `principal` was taken from event `since` on.
    fn took_of(&self, principal: &str, since: usize) -> bool {
        lock(&self.events)
            .get(since..)
            .unwrap_or_default()
            .iter()
            .any(|e| matches!(e, Event::Took { principal: p, .. } if p == principal))
    }
}

/// A Mac's presence and what it reported.
struct Present {
    presence: Presence,
    heard: Arc<Heard>,
    principal: String,
}

impl Present {
    /// The wall time of the row this Mac holds of `principal`, where that is a current seat.
    fn row_at(&self, principal: &str) -> Option<u64> {
        let (_, row) = self.presence.seat(principal)?.row?;
        Some(row.at_ms)
    }

    /// Wait until this Mac holds, of each `(principal, at_ms)` of `want`, the row written at
    /// `at_ms`.
    fn until_rows(&self, want: &[(&str, u64)]) {
        let deadline = Instant::now() + HANG;
        loop {
            let heard = self.heard.mark();
            let missing: Vec<(&str, Option<u64>)> = want
                .iter()
                .map(|(principal, at)| (*principal, *at, self.row_at(principal)))
                .filter(|(_, at, held)| *held != Some(*at))
                .map(|(principal, _, held)| (principal, held))
                .collect();
            if missing.is_empty() {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{} holds no row of {want:?} within the hang detector, but {missing:?}; heard {:#?}",
                self.principal,
                lock(&self.heard.events)
            );
            let events = lock(&self.heard.events);
            if events.len() == heard {
                drop(
                    self.heard
                        .moved
                        .wait_timeout(events, left)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
        }
    }

    /// `of`'s standing here now.
    fn standing(&self, of: &Mac) -> Option<Standing> {
        self.presence.seat(&of.principal).map(|seat| seat.standing)
    }

    /// The number of the row this Mac holds of `of`.
    fn seq_of(&self, of: &Mac) -> u64 {
        let (seq, _) = self
            .presence
            .seat(&of.principal)
            .and_then(|seat| seat.row)
            .expect("a row held");
        seq
    }
}

/// The facts every test Mac reports.
fn facts() -> Facts {
    Facts {
        build: Build::new("0.155.0+gpresence").expect("a build"),
        addresses: vec!["127.0.0.1:7480".parse().expect("an address")],
        membership: Some(Held::membership(&BTreeSet::from([[4; 32]]))),
        tables: None,
        topics: Vec::new(),
    }
}

/// Start `mac`'s presence, as its node service does, and wait until its tail runs and its first
/// row is out (so the next [`step`] is the one its next row waits for).
fn present(mac: &Mac) -> Present {
    present_with(mac, Arc::new(facts))
}

/// [`present`], its rows reading `read`.
fn present_with(mac: &Mac, read: crate::presence::ReadFacts) -> Present {
    let heard = Arc::new(Heard::default());
    let noted = Arc::clone(&heard);
    let presence = Presence::start(
        mac.mesh(),
        PresenceConfig {
            dir: mac.dir.path().join(MESH_DIR),
            facts: read,
            observe: Some(Arc::new(move |event: &Event| noted.note(event))),
        },
    )
    .expect("presence starts");
    heard.until("the tail runs and the first row is out", 0, |events| {
        events.iter().any(|e| matches!(e, Event::Tailing { .. }))
            && events.iter().any(|e| matches!(e, Event::Published { .. }))
    });
    Present {
        presence,
        heard,
        principal: mac.principal.clone(),
    }
}

/// Move the wall clock and then the links' clock `by`: each running publisher, rung by the
/// links' clock, writes its row at the new wall time.
fn step(lab: &Lab, by: Duration) {
    lab.age(u64::try_from(by.as_millis()).expect("a test's step"));
    lab.clock.advance(by);
}

fn wall(lab: &Lab) -> u64 {
    lab.wall.load(Ordering::Acquire)
}

/// One period passes, and each of `readers` holds the row each of `writers` wrote at it.
fn round(lab: &Lab, readers: &[&Present], writers: &[&str]) -> u64 {
    step(lab, EVERY);
    let at = wall(lab);
    let want: Vec<(&str, u64)> = writers.iter().map(|w| (*w, at)).collect();
    for reader in readers {
        reader.until_rows(&want);
    }
    at
}

/// Whether `mac` holds a link to `peer`.
fn linked(mac: &Mac, peer: &Mac) -> bool {
    mac.run
        .as_ref()
        .expect("the Mac runs")
        .table
        .links()
        .iter()
        .any(|link| astream_broker::identity_principal(link.peer().key.bytes()) == peer.principal)
}

/// §10 "Presence", on three Macs: each reads every Mac up from the rows it holds. A Mac whose
/// presence stops, its node service and its links running on (so nothing could have probed it
/// down), reads up for 90 seconds of the reader's clock after its last row, then silent, last
/// seen when that row arrived; the Macs whose rows advance never read down.
#[test]
fn a_mac_that_stops_pushing_goes_from_up_to_last_seen_with_no_probe() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    let all = [a.principal.as_str(), &b.principal, &c.principal];
    let mut ps: Vec<Present> = macs.iter().map(present).collect();
    let c_last = round(&lab, &ps.iter().collect::<Vec<_>>(), &all);
    for p in &ps {
        for mac in &macs {
            assert_eq!(
                p.standing(mac),
                Some(Standing::Up),
                "{} reads {} up",
                p.principal,
                mac.principal
            );
        }
    }
    ps.pop().expect("C's presence").presence.stop();
    let readers: Vec<&Present> = ps.iter().collect();
    for after in [30, 60] {
        round(&lab, &readers, &all[..2]);
        for p in &readers {
            assert_eq!(
                p.standing(c),
                Some(Standing::Up),
                "{after} s after its last row C is up on {}",
                p.principal
            );
            assert_eq!(p.standing(a), Some(Standing::Up));
            assert_eq!(p.standing(b), Some(Standing::Up));
        }
    }
    round(&lab, &readers, &all[..2]);
    for p in &readers {
        assert_eq!(
            p.standing(c),
            Some(Standing::Silent {
                last_seen_ms: Some(c_last)
            }),
            "90 s after its last row C reads silent on {}, last seen when it arrived",
            p.principal
        );
        assert_eq!(
            p.standing(a),
            Some(Standing::Up),
            "a Mac whose row advances is up"
        );
        assert_eq!(p.standing(b), Some(Standing::Up));
    }
    assert!(
        linked(a, c) && linked(b, c),
        "C's links ran on: its standing came from rows alone"
    );
}

/// §11 C3, held topics: a Mac whose row names a topic its sessions hold is a holder on every
/// other Mac, which pushes its topic broadcasts there (`Mesh::holders`, read by the mail links'
/// pass); once its row names none it is no holder. A Mac is never its own holder.
/// NEGATIVE CONTROL: B, whose row names no topic, is no holder on A, though A holds B's rows.
#[test]
fn a_mac_whose_row_names_a_topic_is_a_holder_on_the_others() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 2);
    let (a, b) = (&macs[0], &macs[1]);
    let all = [a.principal.as_str(), &b.principal];
    let topics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec!["build".to_owned()]));
    let held = Arc::clone(&topics);
    let pa = present_with(
        a,
        Arc::new(move || Facts {
            topics: lock(&held).clone(),
            ..facts()
        }),
    );
    let pb = present(b);
    round(&lab, &[&pa, &pb], &all);
    assert_eq!(
        b.mesh().holders(),
        BTreeSet::from([a.principal.clone()]),
        "A names build, so B pushes its broadcasts to A"
    );
    assert_eq!(
        a.mesh().holders(),
        BTreeSet::new(),
        "B names none, and A is not its own holder"
    );
    lock(&topics).clear();
    round(&lab, &[&pa, &pb], &all);
    assert_eq!(b.mesh().holders(), BTreeSet::new(), "A names none now");
    drop((pa, pb));
}

/// §10 "Presence": a restarted Mac's first row wins at once on every other Mac. Its number is in
/// a new incarnation, so each broker takes it above the row it held, and each tail advances the
/// seat with it, the clock unmoved.
#[test]
fn a_restarted_macs_first_row_wins_at_once() {
    let lab = Lab::new();
    let mut macs = lab_of(&lab, 3);
    let all: Vec<String> = macs.iter().map(|m| m.principal.clone()).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let mut ps: Vec<Present> = macs.iter().map(present).collect();
    round(&lab, &ps.iter().collect::<Vec<_>>(), &all);
    let held_before = ps[0].seq_of(&macs[2]);
    ps.pop().expect("C's presence").presence.stop();
    macs[2].stop();
    let readers: Vec<&Present> = ps.iter().collect();
    for _ in 0..3 {
        round(&lab, &readers, &all[..2]);
    }
    assert!(matches!(
        ps[0].standing(&macs[2]),
        Some(Standing::Silent { .. })
    ));
    let since = macs[2].seen.mark();
    macs[2].start(&lab);
    macs[2].until_following(&[&macs[0], &macs[1]], since);
    let c = present(&macs[2]);
    let at = wall(&lab);
    for p in &ps {
        p.until_rows(&[(all[2], at)]);
        assert_eq!(
            p.standing(&macs[2]),
            Some(Standing::Up),
            "C reads up on {} from its first row",
            p.principal
        );
        assert!(
            incarnation(p.seq_of(&macs[2])) > incarnation(held_before),
            "the first row after the restart is in a new incarnation"
        );
    }
    drop(c);
}

/// A removed key's row is refused: a removed Mac leaves every view at once, and the row it goes
/// on writing, which a Mac that still seats it takes, never reaches the tail of the Mac that
/// removed it. A key that is no longer a current seat is refused by the tail too, where its rows
/// stay in the log (a retired key's): read again at a start, its row advances nothing.
#[test]
fn a_removed_keys_row_is_refused() {
    let lab = Lab::new();
    let macs = lab_of(&lab, 3);
    let (a, b, c) = (&macs[0], &macs[1], &macs[2]);
    let all = [a.principal.as_str(), &b.principal, &c.principal];
    let mut ps: Vec<Present> = macs.iter().map(present).collect();
    round(&lab, &ps.iter().collect::<Vec<_>>(), &all);

    a.read(Standings {
        current: [a.key, b.key].into(),
        removed: [c.key].into(),
        ..Standings::default()
    });
    a.follow();
    let since = ps[0].heard.mark();
    assert_eq!(ps[0].standing(c), None, "a removed seat leaves the view");
    // B still seats C and takes the row C goes on writing; B's next row follows it on B's log,
    // so once A holds that row, A was offered C's.
    step(&lab, EVERY);
    ps[1].until_rows(&[(all[2], wall(&lab))]);
    round(&lab, &[&ps[0]], &all[..2]);
    assert!(
        !ps[0].heard.took_of(&c.principal, since),
        "no row of C reaches A's tail"
    );
    assert_eq!(ps[0].standing(c), None);

    // B retires C's key: it is revoked and leaves the view, and its rows stay on B's log.
    b.read(Standings {
        current: [a.key, b.key].into(),
        retired: [c.key].into(),
        ..Standings::default()
    });
    b.follow();
    assert_eq!(ps[1].standing(c), None, "a retired seat leaves the view");
    ps.remove(1).presence.stop();
    let again = present(b);
    again.heard.until("C's held row is refused", 0, |events| {
        events.iter().any(|e| {
            matches!(e, Event::Refused { subject, why: Refusal::NotSeated }
                if subject.contains(&c.principal))
        })
    });
    assert!(
        !again.heard.took_of(&c.principal, 0),
        "the tail took no row of C"
    );
    assert_eq!(again.standing(c), None);
    assert!(again.standing(a).is_some());
}

/// What a Mac that joins after C went silent reads of C and of A: its neighbours send it their
/// newest rows, C's among them, which C wrote 11 minutes before by the wall clock.
fn a_new_mac_reads_a_silent_one(stale_rows_up: bool) -> (Option<Standing>, Option<Standing>) {
    let lab = Lab::new();
    let mut macs: Vec<Mac> = (1..=4).map(|tag| Mac::new(&lab, tag)).collect();
    let everyone = seating(&macs.iter().collect::<Vec<_>>());
    for mac in &mut macs[..3] {
        mac.read(everyone.clone());
        mac.start(&lab);
    }
    for mac in &macs[..3] {
        let peers: Vec<&Mac> = macs[..3].iter().filter(|m| m.key != mac.key).collect();
        mac.until_following(&peers, 0);
    }
    let all: Vec<String> = macs.iter().map(|m| m.principal.clone()).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let mut ps: Vec<Present> = macs[..3].iter().map(present).collect();
    let c_last = round(&lab, &ps.iter().collect::<Vec<_>>(), &all[..3]);
    ps.pop().expect("C's presence").presence.stop();
    macs[2].stop();
    let readers: Vec<&Present> = ps.iter().collect();
    let mut last = c_last;
    for _ in 0..22 {
        last = round(&lab, &readers, &all[..2]);
    }
    // D starts seating only itself, so its tail runs before any row reaches it; then it seats
    // every Mac, and its relays read A's and B's logs from their newest rows.
    let mut d = macs.pop().expect("D");
    d.read(seating(&[&d]));
    d.start(&lab);
    STALE_ROWS_UP.with(|on| on.set(stale_rows_up));
    let pd = present(&d);
    STALE_ROWS_UP.with(|on| on.set(false));
    d.read(everyone);
    d.follow();
    pd.until_rows(&[(all[0], last), (all[1], last), (all[2], c_last)]);
    let read = (pd.standing(&macs[2]), pd.standing(&macs[0]));
    drop(pd);
    d.stop();
    read
}

/// The freshness rule end to end: a Mac that joins reads a Mac silent for 11 minutes as silent,
/// though that Mac's newest row reaches it after its tail started, and a Mac whose row is fresh
/// as up.
#[test]
fn a_new_mac_reads_a_silent_macs_newest_row_as_silent() {
    let (c, a) = a_new_mac_reads_a_silent_one(false);
    assert_eq!(c, Some(Standing::Silent { last_seen_ms: None }));
    assert_eq!(a, Some(Standing::Up));
}

/// NEGATIVE CONTROL (`STALE_ROWS_UP`): with the freshness rule off, the joining Mac reads the
/// silent Mac up.
#[test]
fn with_stale_rows_up_a_new_mac_reads_a_silent_mac_up() {
    let (c, _) = a_new_mac_reads_a_silent_one(true);
    assert_eq!(c, Some(Standing::Up));
}

/// Last seen persists per seat: the seen file is written at most once per 10 minutes, so a
/// reader that restarts reads a seat that went silent before it as silent since a time within
/// 10 minutes of when it was last seen, with no row of it arriving since; once it writes again it
/// reads up.
#[test]
fn last_seen_survives_the_readers_restart() {
    let lab = Lab::new();
    let mut macs = lab_of(&lab, 2);
    let all: Vec<String> = macs.iter().map(|m| m.principal.clone()).collect();
    let both = [all[0].as_str(), &all[1]];
    let mut ps: Vec<Present> = macs.iter().map(present).collect();
    let mut b_last = 0;
    for _ in 0..21 {
        b_last = round(&lab, &ps.iter().collect::<Vec<_>>(), &both);
    }
    // 10.5 minutes from the first row: one write at the first advance and one 10 minutes after.
    let writes = lock(&ps[0].heard.events)
        .iter()
        .filter(|e| matches!(e, Event::Wrote { .. }))
        .count();
    assert_eq!(writes, 2, "one write per 10 minutes");
    let written = crate::presence::read_seen(&macs[0].dir.path().join(MESH_DIR));
    let b_written = *written.get(&all[1]).expect("B's line");
    assert!(
        b_written <= b_last && b_last - b_written <= 10 * 60 * 1000,
        "within ten minutes"
    );
    ps.pop().expect("B's presence").presence.stop();
    ps.pop().expect("A's presence").presence.stop();
    macs[0].stop();
    step(&lab, Duration::from_secs(3 * 24 * 60 * 60));
    let since = macs[0].seen.mark();
    macs[0].start(&lab);
    macs[0].until_following(&[&macs[1]], since);
    let a = present(&macs[0]);
    assert_eq!(
        a.standing(&macs[1]),
        Some(Standing::Silent {
            last_seen_ms: Some(b_written)
        }),
        "silent for 3 days, since what A wrote"
    );
    let b = present(&macs[1]);
    a.until_rows(&[(&all[1], wall(&lab))]);
    assert_eq!(a.standing(&macs[1]), Some(Standing::Up));
    drop(b);
}

/// A lab's record store file reads as the records it holds: the digest of their ids, and their
/// count.
#[test]
fn a_record_file_reads_as_the_records_it_holds() {
    let (_, genesis) = super::founded(&[(1, [1; 32]), (2, [2; 32])]);
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = dir.path().join("records");
    std::fs::write(
        &file,
        aterm_secrets::membership_log::encode_records([&genesis]),
    )
    .expect("the store file");
    assert_eq!(
        crate::presence::membership_of_records(&file),
        Some(Held::membership(&BTreeSet::from([*genesis.id()])))
    );
}
