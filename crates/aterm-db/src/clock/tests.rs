// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;
/// A fixed wall clock (ms): day 20,000 since the Unix epoch, at noon.
const NOW: u64 = 20_000 * DAY + 12 * HOUR;

/// A reading of `offset_ms` (`my − peer`) taken over a one-second round at `at`.
fn reading(offset_ms: i64, at: u64) -> Reading {
    Reading {
        lo_ms: offset_ms - 1000,
        hi_ms: offset_ms,
        at_ms: at,
        source: Source::Mark,
    }
}

fn machine(n: u8) -> [u8; 16] {
    [n; 16]
}

/// Members 1 ..= `n` and this Mac (0), each named by a recipient of its own.
fn members(n: u8) -> BTreeMap<[u8; 16], String> {
    (0..=n)
        .map(|m| (machine(m), format!("age1member{m}")))
        .collect()
}

fn judge(readings: &[(u8, i64)], last: Standing) -> Judgement {
    let readings: BTreeMap<_, _> = readings
        .iter()
        .map(|(m, offset)| (machine(*m), reading(*offset, NOW - MIN)))
        .collect();
    standing(
        &readings,
        &members(u8::try_from(readings.len()).unwrap()),
        &machine(0),
        last,
        NOW,
    )
}

/// A mark's one spelling bounds this Mac's offset by the round's span, both ways, whatever the
/// delays; a span over `MAX_SPAN` gives no reading; a list with two marks gives none; a mark with
/// any other tail is an ordinary table ID.
///
/// Negative control: with `read_mark` not checking the zero tail, the non-canonical ID was taken
/// as a mark (`left: Several`, `right: One(..)`).
#[test]
fn a_mark_bounds_the_offset_by_the_round_span() {
    let peer = NOW - 2 * DAY;
    let id = mark(peer);
    assert_eq!(&id[..16], MARK_PREFIX);
    assert_eq!(read_mark(&id), Some(peer));
    let read = Reading::from_mark(peer, NOW, Duration::from_secs(3)).expect("a reading");
    assert_eq!(
        (read.lo_ms, read.hi_ms, read.at_ms),
        (ms(2 * DAY) - 3000, ms(2 * DAY), NOW)
    );
    assert_eq!(read.class(Standing::Unknown), Class::Ahead);
    assert!(Reading::from_mark(peer, NOW, MAX_SPAN).is_some());
    assert_eq!(
        Reading::from_mark(peer, NOW, MAX_SPAN + Duration::from_millis(1)),
        None
    );

    let table = [7; 32];
    let mut tail = mark(NOW);
    tail[31] = 1;
    let (tables, heard) = split(&BTreeSet::from([table, mark(NOW), tail]));
    assert_eq!(heard, Heard::One(NOW));
    assert_eq!(tables, BTreeSet::from([table, tail]));
    let (tables, heard) = split(&BTreeSet::from([table, mark(NOW), mark(NOW + 1)]));
    assert_eq!((tables, heard), (BTreeSet::from([table]), Heard::Several));
    assert_eq!(split(&BTreeSet::from([table])).1, Heard::Nothing);
}

/// B and C agree with A's clock and L says A runs three days ahead: A agrees and names L as
/// skewed. Two liars of three make A Off, ahead, by at least their smaller figure.
///
/// Negative control: with Off on any single dissent (`2 * a > n` as `a > 0`), A went Off on L
/// alone (`left: Off { ahead: true }`, `right: Agrees`).
#[test]
fn one_liar_among_three_moves_no_standing() {
    let liar = ms(3 * DAY);
    let one = judge(&[(1, 0), (2, 0), (3, liar)], Standing::Unknown);
    assert_eq!(one.standing, Standing::Agrees);
    assert_eq!((one.against, one.of), (1, 3));
    assert_eq!(
        one.skewed,
        vec![Skewed {
            machine: machine(3),
            recipient: "age1member3".to_owned(),
            until_ms: NOW - MIN + FRESH_MS,
            ahead_ms: liar,
        }]
    );
    assert_eq!(one.refusal(), None);

    let two = judge(
        &[(1, 0), (2, liar), (3, liar + ms(HOUR))],
        Standing::Unknown,
    );
    assert_eq!(two.standing, Standing::Off { ahead: true });
    assert_eq!((two.by_ms, two.against, two.of), (3 * DAY - 1000, 2, 3));
    assert!(two.skewed.is_empty(), "an Off Mac names nobody skewed");
    assert!(two.refusal().is_some());
}

/// A tie is Split, never Agrees: a Mac whose clock is wrong next to one liar and one honest peer
/// leases nothing on the liar's word.
///
/// Negative control: with a tie counted as Agrees (`2 * g > n` as `2 * g >= n`), the tie read
/// Agrees (`left: Agrees`, `right: Split`).
#[test]
fn a_tie_is_split() {
    let tie = judge(&[(1, ms(2 * DAY)), (2, 0)], Standing::Unknown);
    assert_eq!(tie.standing, Standing::Split);
    assert_eq!((tie.against, tie.of), (1, 2));
    assert_eq!(tie.skewed.len(), 1);
    assert_eq!(tie.skewed[0].machine, machine(1));
}

/// A Mac with no readings, only stale ones, or ones dated in its future (a clock set back since)
/// stands Unknown and names nobody skewed; an Undecided reading does not count either.
///
/// Negative control: with `fresh` not refusing a reading dated in the future (`checked_sub` as
/// `saturating_sub`), the future readings voted (`left: Off { ahead: true }`, `right: Unknown`).
#[test]
fn no_fresh_decisive_reading_is_unknown() {
    let all = members(2);
    let me = machine(0);
    let judged = |readings: BTreeMap<[u8; 16], Reading>| {
        standing(&readings, &all, &me, Standing::Unknown, NOW).standing
    };
    assert_eq!(judged(BTreeMap::new()), Standing::Unknown);
    let at = |at| {
        (1..=2)
            .map(|m| (machine(m), reading(ms(2 * DAY), at)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(judged(at(NOW - FRESH_MS - 1)), Standing::Unknown, "stale");
    assert_eq!(judged(at(NOW + MIN)), Standing::Unknown, "in the future");
    assert_eq!(
        judged(at(NOW - FRESH_MS)),
        Standing::Off { ahead: true },
        "fresh"
    );
    let straddles = Reading {
        lo_ms: -T_MS * 2,
        hi_ms: T_MS * 2,
        at_ms: NOW,
        source: Source::Ring,
    };
    assert_eq!(straddles.class(Standing::Unknown), Class::Undecided);
    assert_eq!(
        judged(BTreeMap::from([(machine(1), straddles)])),
        Standing::Unknown
    );
    // This Mac's own reading, and a non-member's, never vote.
    let own = BTreeMap::from([
        (me, reading(ms(2 * DAY), NOW)),
        (machine(9), reading(ms(2 * DAY), NOW)),
    ]);
    assert_eq!(judged(own), Standing::Unknown);
}

/// Off holds until a majority reads the clock within half the tolerance: three quarters of it
/// keep an Off Mac Off, while the same readings make a Mac that was not Off agree; a quarter
/// lets it leave. Off behind holds the same way.
///
/// Negative control: with the class ignoring the last standing (`T_MS` both ways), the Mac left
/// Off at three quarters of the tolerance (`left: Agrees`, `right: Off { ahead: true }`).
#[test]
fn leaving_off_needs_half_the_tolerance() {
    let three_quarters = T_MS * 3 / 4;
    let quarter = T_MS / 4;
    let off = Standing::Off { ahead: true };
    let past = T_MS + T_MS / 2;
    assert_eq!(
        judge(&[(1, past), (2, past)], Standing::Unknown).standing,
        off
    );
    assert_eq!(
        judge(&[(1, three_quarters), (2, three_quarters)], off).standing,
        off
    );
    assert_eq!(
        judge(
            &[(1, three_quarters), (2, three_quarters)],
            Standing::Agrees
        )
        .standing,
        Standing::Agrees
    );
    assert_eq!(
        judge(&[(1, quarter), (2, quarter)], off).standing,
        Standing::Agrees
    );
    let behind = Standing::Off { ahead: false };
    assert_eq!(
        judge(&[(1, -three_quarters), (2, -three_quarters)], behind).standing,
        behind
    );
}

/// A Mac set from 20 minutes fast to 7 stays Off (its peers read it past half the tolerance),
/// while each peer, agreeing with its own peers, reads it within the tolerance: no peer names it
/// skewed, yet every one that counted against it holds its home accounts. A peer whose clock
/// stands a little from the majority's, reading the Off Mac just inside half the tolerance, holds
/// too; one reading it within a quarter holds nothing, and an Off Mac holds for nobody.
///
/// Negative controls: with the holds taken from the decisive readings that do not agree (as
/// `skewed` is), the peer reading the Off Mac 7 minutes behind held nothing (`left: []`, `right:
/// [[1, 1, ..]]`); with the hold line at half the tolerance (`HOLD_T_MS` as `T_MS / 2`), the peer
/// reading it 4.9 minutes behind held nothing (`a peer just inside T/2 holds`, `left: []`,
/// `right: [[1, ..]]`).
#[test]
fn every_peer_that_counted_an_off_mac_holds_for_it() {
    let seven = ms(7 * MIN);
    let off = Standing::Off { ahead: true };
    let a = judge(&[(1, seven), (2, seven)], off);
    assert_eq!(a.standing, off, "A, 7 minutes fast, stays Off");
    assert!(
        a.held.is_empty() && a.skewed.is_empty(),
        "an Off Mac holds for nobody"
    );
    // B (Mac 0 here) agrees with C (Mac 2) and reads A (Mac 1) 7 minutes behind it.
    let b = judge(&[(1, -seven), (2, 0)], Standing::Agrees);
    assert_eq!(b.standing, Standing::Agrees);
    assert!(b.skewed.is_empty(), "within the tolerance: nobody named");
    let held = |judged: &Judgement| {
        judged
            .held
            .iter()
            .map(|skewed| skewed.machine)
            .collect::<Vec<_>>()
    };
    assert_eq!(held(&b), vec![machine(1)]);
    assert_eq!(b.held[0].until_ms, NOW - MIN + HOLD_MS);
    // A stays Off on C and D at 5.1 minutes; B, 12 seconds from them, reads A at 4.9.
    let a = judge(
        &[
            (1, ms(51 * MIN / 10)),
            (2, ms(51 * MIN / 10)),
            (3, ms(49 * MIN / 10)),
        ],
        off,
    );
    assert_eq!(a.standing, off);
    let b = judge(&[(1, -ms(49 * MIN / 10)), (2, 0)], Standing::Agrees);
    assert_eq!(held(&b), vec![machine(1)], "a peer just inside T/2 holds");
    let near = judge(&[(1, -ms(2 * MIN)), (2, 0)], Standing::Agrees);
    assert!(near.held.is_empty(), "within a quarter: no hold");
}

/// Silence keeps Off: a Mac its peers last read Off, whose readings have all gone stale (a sleep),
/// or are dated in its future (its clock set back since), or are gone, stays Off, its refusal
/// saying what it last stood on, until a fresh reading moves it; Off with no other member, or
/// any other standing with no fresh reading, is Unknown.
///
/// Negative control: with no fresh decisive reading judged Unknown whatever the last standing
/// (the Off arm of `n == 0` left out), the Off Mac woke from a sleep Unknown (`asleep`, `left:
/// Unknown`, `right: Off { ahead: true }`).
#[test]
fn silence_keeps_an_off_mac_off() {
    let off = Standing::Off { ahead: true };
    let all = members(2);
    let me = machine(0);
    let at = |at| {
        (1..=2)
            .map(|m| (machine(m), reading(ms(2 * DAY), at)))
            .collect::<BTreeMap<_, _>>()
    };
    let judged = |readings: &BTreeMap<[u8; 16], Reading>, last, members| {
        standing(readings, members, &me, last, NOW)
    };
    let asleep = judged(&at(NOW - FRESH_MS - 1), off, &all);
    assert_eq!(asleep.standing, off, "asleep");
    assert!(asleep.silent);
    assert_eq!(
        (asleep.by_ms, asleep.against, asleep.of),
        (2 * DAY - 1000, 2, 2)
    );
    assert_eq!(
        asleep.refusal().as_deref(),
        Some(
            "this Mac's clock read at least 1 d 23 h ahead of 2 of the 2 Macs it last heard; it \
             writes no account claim until a peer reads the clock agreeing"
        )
    );
    assert!(asleep.held.is_empty(), "an Off Mac holds for nobody");
    assert_eq!(judged(&at(NOW + MIN), off, &all).standing, off, "set back");
    let gone = judged(&BTreeMap::new(), off, &all);
    assert_eq!((gone.standing, gone.by_ms, gone.of), (off, 0, 0));
    let alone = BTreeMap::from([(me, "age1member0".to_owned())]);
    assert_eq!(
        judged(&BTreeMap::new(), off, &alone).standing,
        Standing::Unknown,
        "no other member"
    );
    for last in [Standing::Agrees, Standing::Split, Standing::Unknown] {
        let quiet = judged(&at(NOW - FRESH_MS - 1), last, &all);
        assert_eq!((quiet.standing, quiet.silent), (Standing::Unknown, false));
    }
    // A fresh reading moves it: agreeing within half the tolerance, it leaves Off.
    let fresh = (1..=2)
        .map(|m| (machine(m), reading(0, NOW - MIN)))
        .collect();
    let woke = judged(&fresh, off, &all);
    assert_eq!((woke.standing, woke.silent), (Standing::Agrees, false));
}

/// A peer stays held a day past the reading that held it, as long as a home claim row it wrote
/// would last, though that reading no longer counts toward a standing: half an hour gone, or 23
/// hours. A reading over a day old, or one dated in the future, holds nothing.
///
/// Negative control: with the holds counting only fresh readings (`holding` as `holds` and
/// `fresh`), the peer read two hours ago was not held (`7200000`, `left: []`, `right: [([1, ..],
/// 1728122400000)]`).
#[test]
fn a_held_peer_stays_held_a_day_past_its_reading() {
    let all = members(1);
    let me = machine(0);
    let held = |at: u64| {
        let readings = BTreeMap::from([(machine(1), reading(-ms(DAY), at))]);
        standing(&readings, &all, &me, Standing::Unknown, NOW)
            .held
            .iter()
            .map(|skewed| (skewed.machine, skewed.until_ms))
            .collect::<Vec<_>>()
    };
    for ago in [2 * HOUR, 23 * HOUR, HOLD_MS] {
        assert_eq!(
            held(NOW - ago),
            vec![(machine(1), NOW - ago + HOLD_MS)],
            "{ago}"
        );
    }
    assert!(held(NOW - HOLD_MS - 1).is_empty(), "over a day old");
    assert!(held(NOW + MIN).is_empty(), "dated in the future");
}

/// The store's one spelling round-trips; a missing file, a damaged one, a truncated one, one with
/// machines out of order, or one with an unknown standing reads as no readings.
///
/// Negative control: with `decode` not re-encoding for its one spelling, the unsorted file was
/// read (`left: Some(State { last: Off { ahead: false }, readings: {..} })`, `right: None`).
#[test]
fn clock_state_round_trips_and_a_damaged_one_says_nothing() {
    let (_root, dir) = tempfile_dir();
    assert_eq!(State::read(&dir), State::default(), "no file");
    let state = State {
        last: Standing::Off { ahead: false },
        readings: BTreeMap::from([
            (machine(2), reading(-ms(DAY), NOW)),
            (
                machine(1),
                Reading {
                    lo_ms: -5,
                    hi_ms: 7,
                    at_ms: NOW - 1,
                    source: Source::Ring,
                },
            ),
        ]),
    };
    state.write(&dir).unwrap();
    assert_eq!(State::read(&dir), state);
    let bytes = state.encode();
    assert_eq!(bytes.len(), 5 + 2 + 1 + 2 * ENTRY_LEN);
    let mut unsorted = bytes[..8].to_vec();
    unsorted.extend_from_slice(&bytes[8 + ENTRY_LEN..]);
    unsorted.extend_from_slice(&bytes[8..8 + ENTRY_LEN]);
    let mut unknown = bytes.clone();
    unknown[7] = 9;
    let mut source = bytes.clone();
    source[8 + ENTRY_LEN - 1] = 2;
    for damaged in [
        &bytes[..bytes.len() - 1],
        &unsorted[..],
        &unknown[..],
        &source[..],
        b"ATCK2\0\0\0",
    ] {
        assert_eq!(State::decode(damaged), None);
        aterm_uds::private_file::write_private(&dir, STATE_NAME, damaged).unwrap();
        assert_eq!(State::read(&dir), State::default());
    }
}

/// The runtime's store writes the file on its first judgment, when a reading's class or the
/// standing changes, and otherwise at most once a `keep` once a reading changed; a reading from a
/// round that authenticated before the held one's is not taken, a later round's is though a clock
/// set back dates it earlier, and a reading of a Mac that left the members is dropped.
///
/// Negative controls: with `tick` writing only every `keep` (`moved` ignored), the class change
/// went unwritten (`assertion failed: clocks.tick(&dir, &all, &me, NOW + 4000, second(2), keep):
/// a class change writes at once`); with `observe` ordering readings by their wall time (the held
/// one kept while `held.at_ms > reading.at_ms`), the reading of a round that authenticated
/// earlier but read a later wall time replaced the later round's (`an earlier round, finished
/// later`, `left: 0`, `right: -5`).
#[test]
fn the_store_writes_a_change_at_once_and_otherwise_once_a_keep() {
    let (_root, dir) = tempfile_dir();
    let keep = Duration::from_secs(600);
    let all = members(2);
    let me = machine(0);
    let start = Instant::now();
    // When each reading's round authenticated.
    let round = |n: u64| start + Duration::from_millis(n);
    let mut clocks = Clocks::open(&dir);
    clocks.observe(machine(1), reading(0, NOW), round(0));
    assert!(clocks.tick(&dir, &all, &me, NOW, start, keep), "first");
    assert_eq!(State::read(&dir).last, Standing::Agrees);

    clocks.observe(machine(1), reading(-5, NOW + 1000), round(2));
    assert!(
        !clocks.tick(
            &dir,
            &all,
            &me,
            NOW + 1000,
            start + Duration::from_secs(1),
            keep
        ),
        "the same class waits for keep"
    );
    clocks.observe(machine(1), reading(0, NOW + 1500), round(1));
    assert_eq!(
        clocks.state().readings[&machine(1)].hi_ms,
        -5,
        "an earlier round, finished later"
    );
    clocks.observe(machine(1), reading(-6, NOW - 1000), round(3));
    assert_eq!(
        clocks.state().readings[&machine(1)].hi_ms,
        -6,
        "a later round on a clock set back"
    );
    assert!(
        clocks.tick(&dir, &all, &me, NOW + 2000, start + keep, keep),
        "keep passed with a reading changed"
    );
    assert_eq!(State::read(&dir).readings[&machine(1)].hi_ms, -6);
    let second = |n: u64| start + keep + Duration::from_secs(n);
    assert!(
        !clocks.tick(&dir, &all, &me, NOW + 3000, second(1), keep),
        "nothing changed"
    );

    clocks.observe(machine(2), reading(ms(2 * DAY), NOW + 4000), round(4));
    assert!(
        clocks.tick(&dir, &all, &me, NOW + 4000, second(2), keep),
        "a class change writes at once"
    );
    assert_eq!(State::read(&dir).last, Standing::Split);
    let fewer = members(1);
    assert!(clocks.tick(&dir, &fewer, &me, NOW + 5000, second(3), keep));
    let kept = State::read(&dir);
    assert_eq!(kept.last, Standing::Agrees);
    assert!(!kept.readings.contains_key(&machine(2)));
    // A reopened store starts from the file.
    assert_eq!(Clocks::open(&dir).state(), &kept);
}

/// A private scratch directory of this test's own, removed when the guard drops.
fn tempfile_dir() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("table-claims");
    aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
    (root, dir)
}

/// The model's offset for a reading of class `class` (1 Agree, 2 Ahead, 3 Behind): `my − peer`,
/// two days off either way.
fn class_offset(class: i64) -> i64 {
    match class {
        1 => 0,
        2 => ms(2 * DAY),
        3 => -ms(2 * DAY),
        _ => panic!("no reading of class {class}"),
    }
}

/// A's clock's offset from true time for the model's `skew` (0 right, 1 ahead, 2 behind).
fn skew_offset(skew: i64) -> i64 {
    match skew {
        0 => 0,
        1 => ms(2 * DAY),
        2 => -ms(2 * DAY),
        _ => panic!("no skew {skew}"),
    }
}

/// The wall time `offset` from `base`.
fn shift(base: u64, offset: i64) -> u64 {
    base.checked_add_signed(offset).expect("a wall time")
}

/// The model's value of a code class: 1 Agree, 2 Ahead, 3 Behind; Undecided is no model value.
const fn class_value(class: Class) -> i64 {
    match class {
        Class::Agree => 1,
        Class::Ahead => 2,
        Class::Behind => 3,
        Class::Undecided => 9,
    }
}

/// The standing of the model's value (0 Unknown, 1 Agrees, 2 Split, 3 Off, ahead: its direction
/// moves no class of the model's readings, two days off or none).
fn standing_of(st: i64) -> Standing {
    match st {
        0 => Standing::Unknown,
        1 => Standing::Agrees,
        2 => Standing::Split,
        3 => Standing::Off { ahead: true },
        _ => panic!("no standing {st}"),
    }
}

/// The model's value of a standing: 0 Unknown, 1 Agrees, 2 Split, 3 Off.
const fn standing_value(standing: Standing) -> i64 {
    match standing {
        Standing::Unknown => 0,
        Standing::Agrees => 1,
        Standing::Split => 2,
        Standing::Off { .. } => 3,
    }
}

/// A Commons of four writers for `DbClaimClock`: A (Mac 0) and its peers B, C and the liar L
/// (Macs 1 to 3), the rows A has written, and whether A's `write_at` lands on each set of readings
/// (asked once per set, the claims table being slow to open).
struct ClockBench {
    lab: crate::table::tests::Lab,
    members: BTreeMap<[u8; 16], String>,
    me: [u8; 16],
    peers: [[u8; 16]; 3],
    rows: u64,
    lands: BTreeMap<[i64; 4], bool>,
}

impl ClockBench {
    fn new() -> Self {
        use aterm_secrets::commons::roster::Role;
        let lab = crate::table::tests::Lab::new(&[Role::Writer; 4]);
        let bindings: Vec<_> = (0..4)
            .map(|n| lab.bootstrap(n).request().unwrap().binding().clone())
            .collect();
        Self {
            members: super::members(bindings.iter()),
            me: *bindings[0].machine(),
            peers: [1, 2, 3].map(|n| *bindings[n].machine()),
            lab,
            rows: 0,
            lands: BTreeMap::new(),
        }
    }

    /// A's held readings of B, C and L in `state`, each taken a minute before `now_ms`.
    fn readings(
        &self,
        state: &aterm_spec::interp::State,
        now_ms: u64,
    ) -> BTreeMap<[u8; 16], Reading> {
        ["rb", "rc", "rl"]
            .iter()
            .zip(self.peers)
            .filter(|(var, _)| state[**var] != 0)
            .map(|(var, peer)| (peer, reading(class_offset(state[*var]), now_ms - MIN)))
            .collect()
    }

    /// Whether A's `write_at` lands with the readings and the last standing of `state` in its
    /// `clock.state`.
    fn lands(&mut self, state: &aterm_spec::interp::State) -> bool {
        let key = [state["rb"], state["rc"], state["rl"], state["st"]];
        if let Some(lands) = self.lands.get(&key) {
            return *lands;
        }
        let now_s = NOW / 1000;
        let dir = super::dir(&self.lab.bootstrap(0)).unwrap();
        State {
            last: standing_of(state["st"]),
            readings: self.readings(state, NOW),
        }
        .write(&dir)
        .unwrap();
        let body = aterm_json::json!({
            "counter": self.rows + 1,
            "until": now_s + 24 * 3600,
            "claims": [],
        });
        let lands = crate::accounts::claims::write_at(
            &self.lab.paths[0],
            &body,
            now_s,
            crate::accounts::claims::Caps::default(),
        )
        .is_ok();
        self.rows += u64::from(lands);
        self.lands.insert(key, lands);
        lands
    }
}

/// The code's step for each `DbClaimClock` action, `Lie` (a choice of the liar's) stepped as the
/// model steps it: an honest peer's reading is what [`Reading::from_mark`] makes of a mark at true time
/// received on A's clock, the liar's of a mark at the time it picks; a reading held across a clock
/// step, or forgotten, is one [`Reading::fresh`] no longer counts; `Judge` is [`standing`] of the
/// readings held; `Write` is whether `accounts::claims::write_at` lands.
pub(crate) fn claim_clock_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    claim_clock_step(&mut ClockBench::new(), state, action)
}

fn claim_clock_step(
    bench: &mut ClockBench,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    // A's clock in `state`, its readings taken on it.
    let mine = shift(NOW, skew_offset(state["skew"]));
    let mark = |peer: u64| {
        let read = Reading::from_mark(peer, mine, Duration::from_millis(1000)).unwrap();
        class_value(read.class(Standing::Unknown))
    };
    let keeps = |var: &str, now_ms: u64| {
        state[var] != 0 && reading(class_offset(state[var]), mine).fresh(now_ms)
    };
    match action {
        "Ahead" | "Behind" | "Fix" => {
            let skew = match action {
                "Ahead" => 1,
                "Behind" => 2,
                _ => 0,
            };
            let stepped = shift(NOW, skew_offset(skew));
            next.insert("skew", skew);
            for var in ["rb", "rc", "rl"] {
                if !keeps(var, stepped) {
                    next.insert(var, 0);
                }
            }
            next.insert("dirty", 1);
        }
        "HearB" | "HearC" => {
            next.insert(if action == "HearB" { "rb" } else { "rc" }, mark(NOW));
            next.insert("dirty", 1);
        }
        "Lie" => {
            next.insert("lie", state["lie"] % 3 + 1);
        }
        "HearL" => {
            next.insert("rl", mark(shift(mine, -class_offset(state["lie"]))));
            next.insert("dirty", 1);
        }
        "ForgetB" | "ForgetC" | "ForgetL" => {
            let var = match action {
                "ForgetB" => "rb",
                "ForgetC" => "rc",
                _ => "rl",
            };
            if !keeps(var, mine + FRESH_MS + 1) {
                next.insert(var, 0);
            }
            next.insert("dirty", 1);
        }
        "Judge" => {
            let readings = bench.readings(state, mine);
            let judged = standing(
                &readings,
                &bench.members,
                &bench.me,
                standing_of(state["st"]),
                mine,
            );
            next.insert("st", standing_value(judged.standing));
            next.insert("dirty", 0);
            // The model's record of the last judgement that counted a reading.
            if !readings.is_empty() {
                next.insert("lastoff", i64::from(judged.off()));
            }
        }
        "Write" => {
            if bench.lands(state) && state["st"] == 3 {
                next.insert("woff", 1);
            }
        }
        other => panic!("no DbClaimClock step {other}"),
    }
    next
}

/// Every enabled `DbClaimClock` step of the code is a model successor, and A's write lands
/// exactly where the model enables `Write` (on every judged state, Off ones included).
///
/// Negative controls: the model's own, from A skewed ahead with B reading it Ahead, C none and L
/// Agree, judging Agrees is no successor and breaks `NoBlindAgree`; from A last judged Off with
/// every reading gone, judging Unknown is no successor and breaks `SilenceKeepsOff`. In the code:
/// - with a tie counted as Agrees (`2 * g > n` as `2 * g >= n`), `Judge` from that first state
///   gave st 1, no model successor (`Judge from {"dirty": 1, "lastoff": 0, "lie": 1, "rb": 2,
///   "rc": 0, "rl": 1, "skew": 1, "st": 0, "woff": 0} gave {.., "st": 1, ..}`);
/// - with `write_at` writing whatever the clock (its refusal filtered out), A wrote while Off
///   (`Write from {"dirty": 0, "lastoff": 1, "lie": 1, "rb": 2, "rc": 0, "rl": 0, "skew": 1,
///   "st": 3, "woff": 0}`, `left: true`, `right: false`);
/// - with no fresh reading judged Unknown whatever the last standing (the `Off` arm of `n == 0`
///   left out), `Judge` from the silent Off state gave st 0 (`Judge from {"dirty": 1, "lastoff":
///   1, "lie": 1, "rb": 0, "rc": 0, "rl": 0, "skew": 2, "st": 3, "woff": 0} gave {"dirty": 0,
///   "lastoff": 1, .., "st": 0, ..}`).
#[test]
fn claim_clock_conforms() {
    let model = aterm_models_db::claim_clock_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut bench = ClockBench::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["dirty"] == 0 {
            assert_eq!(
                bench.lands(&state),
                model.action_enabled("Write", &state),
                "Write from {state:?}"
            );
        }
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let successors = model.successors(action.name, &state);
            let next = claim_clock_step(&mut bench, &state, action.name);
            assert!(
                successors.contains(&next),
                "{} from {state:?} gave {next:?}",
                action.name
            );
        }
    }
    let before = aterm_spec::interp::State::from([
        ("skew", 1),
        ("rb", 2),
        ("rc", 0),
        ("rl", 1),
        ("lie", 1),
        ("st", 0),
        ("dirty", 1),
        ("woff", 0),
        ("lastoff", 0),
    ]);
    let mut blind = before.clone();
    blind.insert("st", 1);
    blind.insert("dirty", 0);
    assert!(!model.successors("Judge", &before).contains(&blind));
    assert!(!model.check_invariant("NoBlindAgree", &blind));
    assert_eq!(
        claim_clock_transition(&before, "Judge")["st"],
        2,
        "a tie is Split"
    );
    // Off, every reading gone (a sleep, or the clock set since): judging Unknown is no successor
    // and breaks `SilenceKeepsOff`; the code keeps Off.
    let silent = aterm_spec::interp::State::from([
        ("skew", 0),
        ("rb", 0),
        ("rc", 0),
        ("rl", 0),
        ("lie", 1),
        ("st", 3),
        ("dirty", 1),
        ("woff", 0),
        ("lastoff", 1),
    ]);
    let mut cleared = silent.clone();
    cleared.insert("st", 0);
    cleared.insert("dirty", 0);
    assert!(!model.successors("Judge", &silent).contains(&cleared));
    assert!(!model.check_invariant("SilenceKeepsOff", &cleared));
    assert_eq!(
        claim_clock_transition(&silent, "Judge")["st"],
        3,
        "silence keeps Off"
    );
}
