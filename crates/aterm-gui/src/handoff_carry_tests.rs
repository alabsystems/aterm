// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control carry's own halves: the capture split around the freeze, the
//! tail it picks, the sidecar codec, and a receiver that never panics on
//! what it is handed.

use super::*;

const ROWS: u16 = 24;
const COLS: u16 = 80;

/// A Claude-shaped screen (synthetic): transcript rows over a composer.
fn screen(doc: &[String], rows: u16, cols: u16) -> Vec<String> {
    let t = usize::from(rows) - 5;
    let rule = "─".repeat(usize::from(cols) - 1);
    let mut out: Vec<String> = doc[doc.len().saturating_sub(t)..].to_vec();
    while out.len() < t {
        out.insert(0, String::new());
    }
    out.extend([
        String::new(),
        rule.clone(),
        "❯".to_string(),
        rule,
        "  ? for shortcuts".to_string(),
    ]);
    out
}

fn sync_frame(rows: &[String]) -> Vec<u8> {
    let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
    for (r, text) in rows.iter().enumerate() {
        v.extend_from_slice(format!("\x1b[{};1H\x1b[2K{text}", r + 1).as_bytes());
    }
    v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
    v
}

fn record(id: u64, submitted: bool, arch: ArchMark) -> TurnRecord {
    TurnRecord {
        id,
        started_ms: id * 10,
        dur_ms: 5,
        submitted,
        status: "settled",
        text: format!("turn {id}"),
        screen_hash: id,
        seq: id,
        arch,
        carried: false,
    }
}

/// A session whose app printed `n` rows under origin 7, with a turn marked
/// every ten rows: its engine and ledger, shared the way `SessionCtx` shares
/// them.
fn session(n: usize) -> (Arc<Mutex<Terminal>>, Arc<Mutex<TurnLedger>>) {
    session_every(n, 10)
}

/// [`session`], a turn marked every `every` rows.
fn session_every(n: usize, every: usize) -> (Arc<Mutex<Terminal>>, Arc<Mutex<TurnLedger>>) {
    let mut t = Terminal::new(ROWS, COLS);
    t.set_alt_archive_enabled(true);
    t.set_alt_archive_origin(7);
    t.process(b"\x1b[?1049h");
    let mut ledger = TurnLedger::default();
    let mut doc = Vec::new();
    for i in 0..n {
        if i % every == 0 {
            ledger.push(record(i as u64 + 1, true, ArchMark::of(&t)));
        }
        doc.push(format!("⏺ worker row {i:04} says something"));
        t.process(&sync_frame(&screen(&doc, ROWS, COLS)));
    }
    (Arc::new(Mutex::new(t)), Arc::new(Mutex::new(ledger)))
}

fn capture(
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    differ: bool,
) -> CarrySource {
    let guard = term.lock().unwrap();
    capture_head(3, &guard, term, turns, differ)
}

fn decoded(sources: &[CarrySource]) -> ControlCarry {
    let out = export(sources);
    assert_eq!(out.len(), sources.len());
    decode(&out[0].1).expect("a sidecar this build reads")
}

/// The tail reaches back to the earliest mark among the newest eight
/// submitted turns, and no further (here the turns are far enough apart that
/// the mark is older than the differ's own reach).
#[test]
fn the_tail_starts_after_the_eighth_newest_submitted_turn() {
    let (term, turns) = session_every(400, 30);
    let src = capture(&term, &turns, true);
    let carry = decoded(std::slice::from_ref(&src));
    let archive = carry.archive.expect("carried");
    let ledger = turns.lock().unwrap();
    let marks: Vec<ArchMark> = ledger.records().map(|r| r.arch).collect();
    let eighth = marks[marks.len() - TAIL_TURNS];
    assert_eq!(
        archive.first,
        eighth.last + 1,
        "from the eighth newest mark on"
    );
    let t = term.lock().unwrap();
    assert_eq!(archive.last(), t.alt_archive().last());
    assert_eq!(archive.lost, archive.first - 1);
    assert_eq!(
        carry.turns.len(),
        ledger.len(),
        "the whole ledger rides along"
    );
    assert!(carry.turns.iter().all(|r| r.carried));
}

/// The tail never stops short of what the adopting differ may point back at
/// (`aterm-core` widens it to its reach: the last eight screens of rows), so a
/// scroll-back past the turns' marks after the handoff is recognized as the
/// old process would have. Here the eight turns are close together.
#[test]
fn the_tail_reaches_back_as_far_as_the_differ_can_point() {
    let (term, turns) = session(300);
    let carry = decoded(&[capture(&term, &turns, true)]);
    let archive = carry.archive.unwrap();
    let ledger = turns.lock().unwrap();
    let marks: Vec<ArchMark> = ledger.records().map(|r| r.arch).collect();
    let eighth = marks[marks.len() - TAIL_TURNS];
    let last = term.lock().unwrap().alt_archive().last();
    let reach = last + 1 - 8 * u64::from(ROWS);
    assert!(
        reach < eighth.last + 1,
        "the reach is the further back here"
    );
    assert_eq!(archive.first, reach);
}

/// With no turn the tail is the newest rows, as many as the cap holds.
#[test]
fn with_no_turn_the_newest_rows_are_carried_up_to_the_cap() {
    let (term, turns) = session(120);
    *turns.lock().unwrap() = TurnLedger::default();
    let carry = decoded(&[capture(&term, &turns, true)]);
    let archive = carry.archive.unwrap();
    let t = term.lock().unwrap();
    assert_eq!(archive.first, t.alt_archive().oldest(), "all of it fits");
    assert_eq!(archive.rows.len(), t.alt_archive().len());
}

/// H1 / the freeze-budget path: a lock another thread keeps, or an archive
/// that moved since the freeze, carries the counters alone — the rows count
/// as lost — and past half the budget the freeze leaves the differ's state
/// out as well. Never an error.
#[test]
fn a_busy_lock_or_a_moved_archive_carries_the_counters_only() {
    let (term, turns) = session(80);
    let last = term.lock().unwrap().alt_archive().last();

    // The engine's lock is held (by this thread, which try_lock sees as busy).
    let src = capture(&term, &turns, true);
    let held = term.lock().unwrap();
    let started = Instant::now();
    let carry = decoded(&[src]);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the export waits only briefly"
    );
    drop(held);
    let archive = carry.archive.unwrap();
    assert!(archive.rows.is_empty());
    assert_eq!((archive.first, archive.lost), (last + 1, last));
    assert!(archive.differ.is_some(), "the freeze's state still rides");
    assert!(!carry.turns.is_empty(), "the ledger was free");

    // The ledger's lock is held: no records, but the archive's rows come —
    // and the carry says which turn ids it no longer vouches for, so the
    // adopted ledger reports them lost (`GAP … events-resync=`).
    let src = capture(&term, &turns, true);
    let held = turns.lock().unwrap();
    let floor = crate::control::turn_ids_minted() + 1;
    let mut carry = decoded(&[src]);
    drop(held);
    assert!(carry.turns.is_empty());
    assert!(
        carry.unheld_below >= floor,
        "{} < {floor}",
        carry.unheld_below
    );
    assert_eq!(carry.ledger().low_id(), Some(carry.unheld_below));
    assert!(!carry.archive.unwrap().rows.is_empty());

    // The archive moved between the freeze and the export.
    let src = capture(&term, &turns, true);
    {
        let mut t = term.lock().unwrap();
        let doc: Vec<String> = (0..90)
            .map(|i| format!("⏺ worker row {i:04} says something"))
            .collect();
        t.process(&sync_frame(&screen(&doc, ROWS, COLS)));
    }
    let archive = decoded(&[src]).archive.unwrap();
    assert!(archive.rows.is_empty(), "the fence moved: counters only");
    assert_eq!(archive.first, last + 1, "the FREEZE's counters");

    // Past half the freeze budget the freeze leaves the differ's state out,
    // and the worker takes it — nothing committed since, so it is the state
    // the freeze would have taken — with the rows, behind the fence.
    let archive = decoded(&[capture(&term, &turns, false)]).archive.unwrap();
    let freeze = capture(&term, &turns, true).head.differ;
    assert!(freeze.is_some());
    assert_eq!(archive.differ, freeze, "the worker took it");
    assert!(
        !archive.rows.is_empty(),
        "the rows still come, behind the fence"
    );

    // …but not once a frame was committed since the freeze.
    let src = capture(&term, &turns, false);
    {
        let mut t = term.lock().unwrap();
        let doc: Vec<String> = (0..95)
            .map(|i| format!("⏺ worker row {i:04} says something"))
            .collect();
        t.process(&sync_frame(&screen(&doc, ROWS, COLS)));
    }
    let archive = decoded(&[src]).archive.unwrap();
    assert!(archive.differ.is_none(), "the differ moved: not taken");
}

/// Over the per-sidecar room the rows go first, then the differ's state,
/// then the ledger's older records — and a room nothing fits in carries
/// nothing.
#[test]
fn a_sidecar_that_does_not_fit_sheds_rows_then_state_then_history() {
    let (term, turns) = session(150);
    let src = capture(&term, &turns, true);
    let turns_v: Vec<TurnRecord> = turns.lock().unwrap().records().cloned().collect();
    let mut archive = src.head.clone();
    assert!(
        term.lock()
            .unwrap()
            .alt_archive_carry_rows(&mut archive, src.fence, 0, usize::MAX)
    );
    let full = encode(&turns_v, 0, &archive).unwrap();
    let no_rows = encode(&turns_v, 0, &counters_only(archive.clone())).unwrap();
    assert!(no_rows.len() < full.len());

    let shed = encode_within(turns_v.clone(), 0, archive.clone(), no_rows.len() as u64).unwrap();
    let c = decode(&shed).unwrap();
    let a = c.archive.unwrap();
    assert!(a.rows.is_empty() && a.differ.is_some());
    assert_eq!(c.turns.len(), turns_v.len());
    assert_eq!(c.unheld_below, 0, "the whole ledger came");

    let tiny = encode_within(turns_v.clone(), 0, archive.clone(), 400).unwrap();
    let c = decode(&tiny).unwrap();
    assert!(c.archive.unwrap().differ.is_none());
    assert!(c.turns.len() < turns_v.len());
    // The records shed are ones the carried ledger no longer vouches for.
    let kept_low = c.turns.first().map_or(u64::MAX, |t| t.id);
    let shed_high = turns_v
        .iter()
        .map(|t| t.id)
        .filter(|&id| id < kept_low)
        .max()
        .unwrap();
    assert_eq!(c.unheld_below, shed_high + 1);

    assert!(encode_within(turns_v, 0, archive, 10).is_none());
}

/// The codec keeps every carried field, unknown fields are skipped and
/// missing ones default, and an unknown version is dropped whole.
#[test]
fn the_sidecar_round_trips_and_tolerates_what_it_does_not_know() {
    let (term, turns) = session(60);
    let bytes = export(&[capture(&term, &turns, true)]).remove(0).1;
    let carry = decode(&bytes).unwrap();
    let t = term.lock().unwrap();
    let (fence, mut archive) = t.alt_archive_carry_head(true);
    let from = tail_from(
        &turns.lock().unwrap().records().cloned().collect::<Vec<_>>(),
        fence,
        archive.differ.as_ref(),
    );
    assert!(t.alt_archive_carry_rows(&mut archive, fence, from, CARRY_ARCHIVE_BYTES));
    assert_eq!(
        carry.archive.as_ref(),
        Some(&archive),
        "every archive field"
    );
    let want: Vec<TurnRecord> = turns
        .lock()
        .unwrap()
        .records()
        .cloned()
        .map(|mut r| {
            r.carried = true;
            r
        })
        .collect();
    assert_eq!(carry.turns, want, "every ledger field");

    // A newer sender's extra fields, and an older one's missing ones.
    let text = String::from_utf8(bytes.clone()).unwrap();
    let extra = text.replacen('{', "{\"future_field\":[1,2,{\"x\":null}],", 1);
    assert_eq!(decode(extra.as_bytes()).unwrap().archive, carry.archive);
    let bare = decode(br#"{"version":1}"#).unwrap();
    assert!(bare.turns.is_empty() && bare.archive.is_none());
    assert!(
        decode(br#"{"version":2,"turns":[]}"#).is_none(),
        "unknown version"
    );
    assert!(decode(b"").is_none());

    // A status word this build does not print drops that record only.
    let odd = text.replacen("\"settled\"", "\"exploded\"", 1);
    assert_eq!(decode(odd.as_bytes()).unwrap().turns.len(), want.len() - 1);
}

#[test]
fn the_stamp_names_exact_bytes() {
    let s = stamp(b"hello");
    assert!(s.starts_with("5 "));
    assert!(stamp_matches(&s, b"hello"));
    assert!(!stamp_matches(&s, b"hellO"));
    assert!(!stamp_matches(&s, b"hello!"));
    assert_eq!(stamp_len(&s), Some(5));
    assert_eq!(stamp_len("5 xyz"), None);
    assert_eq!(stamp_len(&s.to_uppercase()), None);
    assert_eq!(manifest_turn_id(0), None);
    assert_eq!(manifest_turn_id(42), Some(42));
    assert_eq!(manifest_turn_id(MAX_TURN_ID), Some(MAX_TURN_ID));
    assert_eq!(
        manifest_turn_id(MAX_TURN_ID + 1),
        None,
        "past a TOML integer"
    );
    assert_eq!(manifest_turn_id(u64::MAX), None, "past a TOML integer");
}

/// A carried record whose id is past what a manifest can carry is left out
/// (the rest of the ledger comes): an id at `u64::MAX` would use up the
/// turn-id count. The last id a manifest can hold is kept.
#[test]
fn a_turn_id_past_the_manifest_range_is_left_out() {
    let (term, turns) = session(30);
    let bytes = export(&[capture(&term, &turns, true)]).remove(0).1;
    let text = String::from_utf8(bytes).unwrap();
    let ids = |text: &str| -> Vec<u64> {
        decode(text.as_bytes())
            .unwrap()
            .turns
            .iter()
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(ids(&text), vec![1, 11, 21]);
    for (id, kept) in [
        (MAX_TURN_ID, true),
        (MAX_TURN_ID + 1, false),
        (u64::MAX, false),
    ] {
        let spoiled = text.replacen("\"id\":21", &format!("\"id\":{id}"), 1);
        let want = if kept { vec![1, 11, id] } else { vec![1, 11] };
        assert_eq!(ids(&spoiled), want, "id {id}");
        let high = decode(spoiled.as_bytes()).unwrap().high_turn_id().unwrap();
        assert!(high <= MAX_TURN_ID && high.checked_add(1).is_some());
    }
    // An `unheld_below` past the range is held to it too.
    let spoiled = text.replacen(
        "\"unheld_below\":0",
        &format!("\"unheld_below\":{}", u64::MAX),
        1,
    );
    assert_ne!(spoiled, text);
    assert_eq!(
        decode(spoiled.as_bytes()).unwrap().unheld_below,
        MAX_TURN_ID + 1
    );
}

/// A session adopted without its ledger (no sidecar, a bad one) starts with
/// an empty ledger that no longer vouches for any turn id up to the count
/// this process continued from; one carried whole vouches for everything.
#[test]
fn an_adopted_ledger_says_which_turn_ids_it_cannot_vouch_for() {
    let floor = crate::control::turn_ids_minted() + 1;
    let dropped = adopted_ledger(None);
    assert_eq!(dropped.len(), 0);
    assert!(dropped.low_id().is_some_and(|low| low >= floor));
    let mut whole = ControlCarry::default();
    assert_eq!(adopted_ledger(Some(&mut whole)).low_id(), None);
    let mut with_records = ControlCarry {
        turns: vec![record(40, true, ArchMark::default())],
        unheld_below: 30,
        archive: None,
    };
    let ledger = adopted_ledger(Some(&mut with_records));
    assert_eq!(ledger.low_id(), Some(40), "a held record is the low-water");
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// M2, the decoder half: a valid sidecar mangled — bytes flipped, cut
/// short, spliced, numbers swapped for huge ones — either decodes to
/// something the adopting engine installs, or to nothing; and the engine
/// then takes random frames, at random sizes, without a panic.
#[test]
fn a_mangled_sidecar_never_panics_the_adopting_engine() {
    let (term, turns) = session(90);
    let checkpoint = term.lock().unwrap().checkpoint_carry(0).unwrap();
    let good = export(&[capture(&term, &turns, true)]).remove(0).1;
    let text = String::from_utf8(good.clone()).unwrap();
    let numbers: Vec<(usize, usize)> = {
        let b = text.as_bytes();
        let mut v = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i].is_ascii_digit() {
                let s = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                v.push((s, i));
            } else {
                i += 1;
            }
        }
        v
    };
    let (mut some, mut none) = (0, 0);
    for seed in 1..=400u64 {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mangled: Vec<u8> = match r.below(4) {
            0 => {
                let mut b = good.clone();
                for _ in 0..1 + r.below(4) {
                    let at = r.below(b.len() as u64) as usize;
                    b[at] = r.next() as u8;
                }
                b
            }
            1 => good[..r.below(good.len() as u64) as usize].to_vec(),
            2 => {
                let (s, e) = numbers[r.below(numbers.len() as u64) as usize];
                let huge = [
                    "18446744073709551615",
                    "4611686018427387905",
                    "0",
                    "99999999999",
                ];
                let pick = huge[r.below(huge.len() as u64) as usize];
                format!("{}{}{}", &text[..s], pick, &text[e..]).into_bytes()
            }
            _ => {
                let a = r.below(good.len() as u64) as usize;
                let b = r.below(good.len() as u64) as usize;
                let (a, b) = (a.min(b), a.max(b));
                [&good[..a], &good[b..], &good[a..b]].concat()
            }
        };
        let Some(carry) = decode(&mangled) else {
            none += 1;
            continue;
        };
        some += 1;
        let mut t = Terminal::new(ROWS, COLS);
        t.set_alt_archive_enabled(true);
        t.restore_checkpoint(&checkpoint);
        let _ = carry.install(&mut t);
        let mut doc: Vec<String> = (0..90)
            .map(|i| format!("⏺ worker row {i:04} says something"))
            .collect();
        for step in 0..40 {
            match r.below(6) {
                0 => {
                    let rows = 10 + r.below(20) as u16;
                    let cols = 40 + r.below(60) as u16;
                    t.resize(rows, cols);
                }
                1 => doc.truncate(doc.len().saturating_sub(r.below(30) as usize)),
                _ => {
                    for _ in 0..r.below(5) {
                        doc.push(format!("⏺ new row {seed} {step} {}", r.next()));
                    }
                }
            }
            let (rows, cols) = (t.rows(), t.cols());
            t.process(&sync_frame(&screen(&doc, rows, cols)));
            let read = t.alt_archive_read(aterm_core::terminal::AltArchiveQuery::oldest(0, 3));
            if read.back > 0 {
                assert!(
                    read.back_at >= read.oldest && read.back_at <= read.last,
                    "seed {seed}"
                );
            }
        }
    }
    eprintln!("decoded {some}, refused {none}");
    assert!(
        some > 20 && none > 20,
        "both outcomes exercised: {some}/{none}"
    );
}
