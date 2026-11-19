// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::lan::{BrokerHello, PROTOCOL};
use std::cell::{Cell, RefCell};

const SOURCE: &str = "/Applications/aterm.app";
const TEAM: &str = "ABCDE12345";
const OURS: &str = "0.207.0+gfeedbee";
const OLD: &str = "0.206.0+gabc1234";

/// An account's world: its record (`None` in the cell: no record yet), whether it has a place
/// for one, the broker's hello, root's refusals (its source lane's, and the askers' file of
/// [`SOURCE`]), the clock and what was journaled.
struct World {
    place: bool,
    record: RefCell<Option<Vec<u8>>>,
    hello: RefCell<Option<BrokerHello>>,
    hellos: Cell<u32>,
    refusal: RefCell<Option<Vec<u8>>>,
    asker_refusal: RefCell<Option<Vec<u8>>>,
    now: Cell<u64>,
    journaled: RefCell<Vec<String>>,
}

impl World {
    fn new() -> Self {
        Self {
            place: true,
            record: RefCell::new(None),
            hello: RefCell::new(hello(OLD)),
            hellos: Cell::new(0),
            refusal: RefCell::new(None),
            asker_refusal: RefCell::new(None),
            now: Cell::new(1_800_000_000),
            journaled: RefCell::new(Vec::new()),
        }
    }

    fn later(&self, by: Duration) {
        self.now.set(self.now.get() + by.as_secs());
    }

    fn pairs(&self) -> Vec<Seen> {
        Record::parse(self.record.borrow().as_deref().unwrap_or_default()).0
    }
}

impl Expecting for World {
    fn now(&self) -> u64 {
        self.now.get()
    }

    fn record(&self) -> Option<io::Result<Vec<u8>>> {
        self.place.then(|| {
            self.record
                .borrow()
                .clone()
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        })
    }

    fn keep(&self, bytes: &[u8]) -> io::Result<()> {
        *self.record.borrow_mut() = Some(bytes.to_vec());
        Ok(())
    }

    fn hello(&self) -> Option<BrokerHello> {
        self.hellos.set(self.hellos.get() + 1);
        self.hello.borrow().clone()
    }

    fn refusal(&self, path: &Path) -> io::Result<Vec<u8>> {
        let askers = asker_refusal_in(
            Path::new(crate::lan::ADOPTION_REFUSED_ASKERS),
            Path::new(SOURCE),
        );
        let file = if path == Path::new(crate::lan::ADOPTION_REFUSED) {
            &self.refusal
        } else if path == askers {
            &self.asker_refusal
        } else {
            return Err(io::ErrorKind::NotFound.into());
        };
        file.borrow()
            .clone()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn journal(&self, line: &str) {
        self.journaled.borrow_mut().push(line.to_owned());
    }
}

/// A hello in this build's protocol naming `build`.
fn hello(build: &str) -> Option<BrokerHello> {
    Some(BrokerHello {
        protocol: PROTOCOL,
        build: build.to_owned(),
        direct: None,
    })
}

fn reading(image: Part) -> Reading {
    Reading {
        image,
        plist: Part::Current,
    }
}

fn adopting() -> Reading {
    reading(Part::Adopting(TEAM.to_owned()))
}

fn read(world: &World, of: Reading) -> Reading {
    expect(world, of, Path::new(SOURCE), OURS)
}

/// An adopting image is an expectation with a bound: while the broker's hello names the build
/// this account runs it is met and owes nothing; while it names another, the image still reads
/// adopting (owed by nothing) until its bound after this account first saw that ([`LOOKS`] of
/// the broker's looks where the broker recorded a definite failure for the source, else
/// [`LONG_LOOKS`]), and from then on reads [`Part::NotAdopted`], owed with no ask of its own,
/// naming the broker's recorded reason, its lapse journaled once however often it is read. A
/// refusal of another source, or by another broker, is no record. A broker heard
/// serving the expected build ends the pair: the record is cleared and the image reads as
/// `install::read` reads it. MUTANTS (each run): an expectation that never lapses fails the read
/// at [`LAPSE`]; one that lapses at once fails the read one second before it. NEGATIVE CONTROLS:
/// an account with no place for a record, and a reading that is neither adopting nor recorded,
/// ask the broker nothing and are left as they are.
#[test]
fn an_adopting_image_owes_the_elevation_once_the_brokers_looks_pass_on_the_old_build() {
    // The broker serves this build: nothing expected is unmet, and nothing is recorded.
    let world = World::new();
    *world.hello.borrow_mut() = hello(OURS);
    assert_eq!(read(&world, adopting()), adopting());
    assert!(world.record.borrow().is_none(), "nothing to record");

    // The broker serves the old build: not owed within its looks.
    let world = World::new();
    let first = world.now.get();
    let waiting = read(&world, adopting());
    assert_eq!(waiting, adopting());
    assert!(!waiting.owed());
    assert_eq!(
        world.pairs(),
        [Seen {
            broker: OLD.into(),
            expected: OURS.into(),
            first,
            journaled: false,
        }]
    );
    world.later(LAPSE - Duration::from_secs(1));
    let almost = read(&world, adopting());
    assert_eq!(almost, adopting(), "one second before the bound");
    assert!(!almost.owed());
    assert!(world.journaled.borrow().is_empty());

    // The bound: owed, the broker's own reason named, journaled once.
    *world.refusal.borrow_mut() = Some(
        Refusal::new(
            Path::new(SOURCE),
            OLD,
            Kind::Definite,
            "the bundle is on another volume than its image",
        )
        .text()
        .unwrap()
        .into_bytes(),
    );
    world.later(Duration::from_secs(1));
    let lapsed = read(&world, adopting());
    assert_eq!(
        lapsed.image,
        Part::NotAdopted {
            team: TEAM.into(),
            broker: OLD.into(),
            serves: true,
            why: Some("the bundle is on another volume than its image".into()),
        }
    );
    assert!(lapsed.owed());
    assert!(!lapsed.words().contains("adopts by itself"), "{lapsed:?}");
    assert!(
        lapsed.words().contains("another volume"),
        "{}",
        lapsed.words()
    );
    assert_eq!(world.journaled.borrow().len(), 1);
    assert!(world.journaled.borrow()[0].contains(OLD));
    world.later(LAPSE);
    assert!(read(&world, adopting()).owed());
    assert_eq!(world.journaled.borrow().len(), 1, "journaled once");

    // A refusal another source's, or another broker's, is no record: it names no reason, and
    // the pair lapses at the long bound.
    world.later(LONG_LAPSE);
    for refusal in [
        Refusal::new(
            Path::new("/Users//a/aterm.app"),
            OLD,
            Kind::Definite,
            "other source",
        ),
        Refusal::new(
            Path::new(SOURCE),
            "0.1.0+g0000000",
            Kind::Definite,
            "other broker",
        ),
    ] {
        *world.refusal.borrow_mut() = Some(refusal.text().unwrap().into_bytes());
        let lapsed = read(&world, adopting());
        assert!(lapsed.owed());
        assert!(
            matches!(&lapsed.image, Part::NotAdopted { why: None, .. }),
            "{lapsed:?}"
        );
        assert!(lapsed.words().contains("it recorded no reason"));
    }

    // The broker took the update up after all: the image reads current, and the broker heard
    // serving the expected build clears the record.
    *world.hello.borrow_mut() = hello(OURS);
    let current = reading(Part::Current);
    assert_eq!(read(&world, current.clone()), current);
    assert!(world.pairs().is_empty(), "cleared");
    let hellos = world.hellos.get();
    assert_eq!(read(&world, current.clone()), current);
    assert_eq!(world.hellos.get(), hellos, "an empty record asks nothing");

    // A later update the broker does not take up starts its own bound.
    *world.hello.borrow_mut() = hello(OLD);
    assert_eq!(read(&world, adopting()), adopting());
    world.later(LAPSE - Duration::from_secs(1));
    assert!(!read(&world, adopting()).owed());

    // An account with no place for a record is not judged.
    let world = World::new();
    world.now.set(first);
    let mut placeless = World::new();
    placeless.place = false;
    for _ in 0..2 {
        assert_eq!(read(&placeless, adopting()), adopting());
        placeless.later(LONG_LAPSE);
    }
    let other = reading(Part::Differs);
    assert_eq!(read(&world, other.clone()), other);
    assert_eq!(
        (world.hellos.get(), placeless.hellos.get()),
        (0, 0),
        "nothing judged asks the broker nothing"
    );

    // A broker that does not answer judges nothing either.
    let silent = World::new();
    *silent.hello.borrow_mut() = None;
    for _ in 0..2 {
        assert_eq!(read(&silent, adopting()), adopting());
        silent.later(LONG_LAPSE);
    }
    assert!(silent.record.borrow().is_none());
}

/// The decision alone: pairs the broker's build leaves behind are dropped, a clock set back
/// dates the pair at the reader's clock, and the record keeps at most [`PAIRS`].
#[test]
fn a_pair_ends_when_the_broker_moves_on_and_never_lapses_ahead_of_the_clock() {
    let mut record = Record::default();
    assert_eq!(
        judge(&mut record, true, OLD, OURS, 1_000, LAPSE),
        Verdict::Waiting { since: 1_000 }
    );
    // The broker moved on to a third build: the old pair ends, a new one starts.
    assert_eq!(
        judge(&mut record, true, "0.206.5+g5555555", OURS, 5_000, LAPSE),
        Verdict::Waiting { since: 5_000 }
    );
    assert_eq!(record.0.len(), 1);
    // A clock set back.
    assert_eq!(
        judge(&mut record, true, "0.206.5+g5555555", OURS, 10, LAPSE),
        Verdict::Waiting { since: 10 }
    );
    assert_eq!(
        judge(
            &mut record,
            true,
            "0.206.5+g5555555",
            OURS,
            10 + LAPSE.as_secs(),
            LAPSE
        ),
        Verdict::Lapsed {
            since: 10,
            journal: true
        }
    );
    // Not adopting judges nothing, but still ends what the broker left behind.
    assert_eq!(
        judge(&mut record, false, OLD, OURS, 20_000, LAPSE),
        Verdict::Unjudged
    );
    assert!(record.0.is_empty());
    // A build a line cannot carry is not recorded.
    assert_eq!(
        judge(&mut record, true, "has space", OURS, 1, LAPSE),
        Verdict::Unjudged
    );
    for n in 0..PAIRS + 2 {
        judge(&mut record, true, OLD, &format!("0.300.{n}+g1"), 1, LAPSE);
    }
    assert_eq!(record.0.len(), PAIRS);
    assert_eq!(record.0[0].expected, "0.300.2+g1", "the oldest dropped");
    assert_eq!(Record::parse(record.text().as_bytes()), record);
    assert_eq!(
        Record::parse(b"1 0 a b\nnonsense\n2 2 a b\n3 1 a b c\n").0,
        [Seen {
            broker: "a".into(),
            expected: "b".into(),
            first: 1,
            journaled: false,
        }]
    );
}

/// The broker's record of a bundle that asked it counts as the source lane's does, read from
/// that bundle's own file, so no asker's record takes the source's place nor the source's an
/// asker's: where the source lane's record names another bundle, this source's own definite
/// failure still lapses at [`LAPSE`] and names its reason; its file naming another bundle is
/// none. Each path names its own file. NEGATIVE CONTROL (run): with only the source lane's file
/// read, the asker's reason is lost and the pair waits for [`LONG_LAPSE`].
#[test]
fn an_askers_own_record_counts_beside_the_source_lanes() {
    let record = |source: &str, reason: &str| {
        Some(
            Refusal::new(Path::new(source), OLD, Kind::Definite, reason)
                .text()
                .unwrap()
                .into_bytes(),
        )
    };
    let world = World::new();
    *world.refusal.borrow_mut() = record("/Users//b/Applications/aterm.app", "another bundle");
    *world.asker_refusal.borrow_mut() = record(SOURCE, "its strict check refused the bundle");
    assert_eq!(read(&world, adopting()), adopting());
    world.later(LAPSE);
    let lapsed = read(&world, adopting());
    assert_eq!(
        lapsed.image,
        Part::NotAdopted {
            team: TEAM.into(),
            broker: OLD.into(),
            serves: true,
            why: Some("its strict check refused the bundle".into()),
        }
    );

    let world = World::new();
    *world.asker_refusal.borrow_mut() = record("/Users//b/Applications/aterm.app", "not ours");
    assert_eq!(read(&world, adopting()), adopting());
    world.later(LAPSE);
    assert_eq!(
        read(&world, adopting()),
        adopting(),
        "no record of this source"
    );

    let folder = Path::new(crate::lan::ADOPTION_REFUSED_ASKERS);
    let (one, two) = (
        asker_refusal_in(folder, Path::new(SOURCE)),
        asker_refusal_in(folder, Path::new("/Applications/aterm.app/")),
    );
    assert_eq!(one.parent(), Some(folder));
    assert_eq!(one.file_name().unwrap().len(), 64);
    assert_eq!(one, asker_refusal_in(folder, Path::new(SOURCE)));
    assert_ne!(one, two, "each path names its own file");
}

/// The broker's refusal reads back as written, its reason one line; anything else is none, a
/// record of the format before failures had a kind included (which leaves the long bound).
#[test]
fn a_refusal_reads_back_as_the_broker_wrote_it() {
    let refusal = Refusal::new(
        Path::new(SOURCE),
        OLD,
        Kind::NoVerdict,
        "line one\nline two",
    );
    assert_eq!(refusal.reason, "line one line two");
    assert_eq!(
        Refusal::parse(refusal.text().unwrap().as_bytes()),
        Some(refusal.clone())
    );
    let long = Refusal::new(
        Path::new(SOURCE),
        OLD,
        Kind::Definite,
        &"é".repeat(REASON_MAX),
    );
    assert!(long.reason.len() <= REASON_MAX);
    assert!(Refusal::parse(long.text().unwrap().as_bytes()).is_some());
    assert!(
        Refusal::new(Path::new("/a\nb.app"), OLD, Kind::Definite, "x")
            .text()
            .is_none()
    );
    let text = refusal.text().unwrap();
    for bad in [
        text.replacen("refused 2", "refused 1", 1),
        text.replacen("kind=no-verdict", "kind=maybe", 1),
        text.replacen("kind=no-verdict\n", "", 1),
        text.replacen("source=/", "source=", 1),
        format!("{text}extra=1\n"),
        "x".repeat(REFUSAL_MAX + 1),
    ] {
        assert_eq!(Refusal::parse(bad.as_bytes()), None, "{bad}");
    }
}

/// Where the broker recorded no definite failure for the source (its check gave no verdict, or
/// it wrote no record, as a broker from before the records writes none), the expectation stands
/// for the long bound: past the short one the image still reads adopting, and it reads not
/// adopted only at [`LONG_LAPSE`]. A broker that speaks another protocol than this build's serves
/// it nothing: its lapsed image says so, and that the LAN is down until it is renewed.
/// MUTANTS (run): a bound of [`LAPSE`] whatever the record says lapses past the short bound; one
/// that never lapses with no record fails at the long bound.
#[test]
fn an_adoption_with_no_definite_failure_waits_for_the_long_bound() {
    let no_verdict = Refusal::new(
        Path::new(SOURCE),
        OLD,
        Kind::NoVerdict,
        "its strict check gave no verdict: trustd did not answer",
    );
    assert_eq!(bound(Some(&no_verdict)), LONG_LAPSE);
    for (what, refusal) in [("no record", None), ("no verdict", Some(no_verdict))] {
        let world = World::new();
        *world.refusal.borrow_mut() = refusal
            .as_ref()
            .map(|refusal| refusal.text().unwrap().into_bytes());
        assert_eq!(read(&world, adopting()), adopting(), "{what}");
        world.later(LAPSE);
        assert_eq!(
            read(&world, adopting()),
            adopting(),
            "{what}: past the short bound"
        );
        world.later(LONG_LAPSE - LAPSE - Duration::from_secs(1));
        assert_eq!(
            read(&world, adopting()),
            adopting(),
            "{what}: one second before the long bound"
        );
        world.later(Duration::from_secs(1));
        assert_eq!(
            read(&world, adopting()).image,
            Part::NotAdopted {
                team: TEAM.into(),
                broker: OLD.into(),
                serves: true,
                why: refusal.map(|refusal| refusal.reason),
            },
            "{what}: the long bound"
        );
    }

    let world = World::new();
    *world.hello.borrow_mut() = Some(BrokerHello {
        protocol: PROTOCOL + 1,
        build: OLD.into(),
        direct: None,
    });
    assert_eq!(read(&world, adopting()), adopting());
    world.later(LONG_LAPSE);
    let lapsed = read(&world, adopting());
    assert!(
        matches!(&lapsed.image, Part::NotAdopted { serves: false, .. }),
        "{lapsed:?}"
    );
    assert!(lapsed.owed());
    let words = lapsed.words();
    assert!(
        words.contains("another protocol") && words.contains("down"),
        "{words}"
    );
}
