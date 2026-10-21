// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control carry's own halves: the capture split around the freeze, the
//! tail it picks, the sidecar codec, and a receiver that never panics on
//! what it is handed.

use super::*;
use crate::turn_ledger::LEDGER_CAP;

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
        text: format!("turn {id}").into(),
        screen_hash: id,
        seq: id,
        arch,
        // Every caller the wire names, so the round trip carries each.
        by: match id % 3 {
            0 => TurnBy::Harness,
            1 => TurnBy::Ctl,
            _ => TurnBy::Sid(format!("s-{id:020x}")),
        },
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
    capture_head(3, &guard, term, turns, None, differ)
}

/// The turn-id ceiling the export runs under, as the handoff worker takes it.
fn ceiling() -> u64 {
    crate::control::turn_ids_for_handoff()
}

fn decoded(sources: &[CarrySource]) -> ControlCarry {
    let out = export(sources, ceiling());
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

/// The tail never stops short of what the adopting differ may point back at (`aterm-core`
/// widens it to the last eight screens of rows), so a scroll-back past the turns' marks is
/// recognized after the handoff. Here the eight turns are close together.
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

/// A lock another thread keeps, or an archive that moved since the freeze, carries the
/// counters alone (the rows count as lost), and past half the budget the freeze leaves the
/// differ's state out as well. Never an error.
#[test]
fn a_busy_lock_or_a_moved_archive_carries_the_counters_only() {
    let (term, turns) = session(80);
    let last = term.lock().unwrap().alt_archive().last();

    // The engine's lock is held by ANOTHER thread for as long as the export
    // runs: the holder lets go when the test says the export is back, or when
    // its own minute runs out. Its join answers which — the export returned
    // while the lock was still held (it gave up on it), or the minute released
    // it (the export waited for the lock). An order, not a stopwatch; the
    // minute is a hang detector.
    let src = capture(&term, &turns, true);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (back_tx, back_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let term = Arc::clone(&term);
        std::thread::spawn(move || {
            let _held = term.lock().unwrap();
            held_tx.send(()).unwrap();
            back_rx.recv_timeout(Duration::from_secs(60)).is_ok()
        })
    };
    held_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the holder took the lock");
    let carry = decoded(&[src]);
    let _ = back_tx.send(());
    assert!(
        holder.join().unwrap(),
        "the export gave the lock up while another thread still kept it"
    );
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

/// A lock the export gives up on is logged by session and cause, like every other carry loss;
/// a carry that took both locks notes nothing.
#[test]
fn a_lock_the_export_gives_up_on_is_said_by_session_and_cause() {
    let (term, turns) = session(80);
    let losses = |src: CarrySource| {
        let mut noted = Vec::new();
        let out = export_noting(
            &[src],
            ceiling(),
            StatusWords::Current,
            false,
            |id, loss| {
                noted.push((id, loss));
            },
        );
        assert_eq!(out.len(), 1, "the session still hands off");
        noted
    };
    assert_eq!(
        losses(capture(&term, &turns, true)),
        Vec::new(),
        "negative control: both locks were free"
    );

    // The ledger is kept past the export's patience (here, by this thread:
    // `try_lock` does not wait on its own holder).
    let src = capture(&term, &turns, true);
    let held = turns.lock().unwrap();
    let noted = losses(src);
    drop(held);
    match noted.as_slice() {
        [(3, CarryLoss::LedgerBusy { unvouched })] => {
            assert!(*unvouched > crate::control::turn_ids_minted(), "{noted:?}");
        }
        other => panic!("the lost ledger is noted by session: {other:?}"),
    }

    // The engine is kept: the archive crosses as counters alone.
    let src = capture(&term, &turns, true);
    let held = term.lock().unwrap();
    let noted = losses(src);
    drop(held);
    assert_eq!(noted, vec![(3, CarryLoss::TerminalBusy)]);

    // The archive moved after the freeze took its fence.
    let src = capture(&term, &turns, true);
    {
        let mut t = term.lock().unwrap();
        let doc: Vec<String> = (0..90)
            .map(|i| format!("⏺ worker row {i:04} says something"))
            .collect();
        t.process(&sync_frame(&screen(&doc, ROWS, COLS)));
    }
    assert_eq!(losses(src), vec![(3, CarryLoss::ArchiveMoved)]);
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
    let whole = Floors::default();
    let full = encode(&turns_v, whole, &archive).unwrap();
    let no_rows = encode(&turns_v, whole, &counters_only(archive.clone())).unwrap();
    assert!(no_rows.len() < full.len());

    let (shed, none) = encode_within(&turns_v, whole, archive.clone(), no_rows.len() as u64)
        .map(|(c, n)| (c.bytes, n))
        .unwrap();
    assert_eq!(none, 0, "no record shed");
    let c = decode(&shed).unwrap();
    let a = c.archive.unwrap();
    assert!(a.rows.is_empty() && a.differ.is_some());
    assert_eq!(c.turns.len(), turns_v.len());
    assert_eq!(c.unheld_below, 0, "the whole ledger came");

    let (tiny, shed) = encode_within(&turns_v, whole, archive.clone(), 400)
        .map(|(c, n)| (c.bytes, n))
        .unwrap();
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
    // The oldest records, and no more of them than the room needs.
    assert_eq!(shed, turns_v.len() - c.turns.len());
    assert_eq!(
        c.turns[..],
        turns_v[shed..]
            .iter()
            .map(|t| TurnRecord {
                carried: true,
                ..t.clone()
            })
            .collect::<Vec<_>>()[..]
    );
    let mut bare = counters_only(archive.clone());
    bare.differ = None;
    let floors = Floors {
        unheld_below: shed.checked_sub(2).map_or(0, |i| turns_v[i].id + 1),
        ..whole
    };
    let one_fewer = encode(&turns_v[shed - 1..], floors, &bare).unwrap();
    assert!(one_fewer.len() > 400, "one record fewer shed would fit");

    assert!(encode_within(&turns_v, whole, archive, 10).is_none());
}

/// A ROOM SHORT OF THE WHOLE TAIL STILL CARRIES ITS NEWEST ROWS
/// (ATERM-KI-1015-2, U30). The rows were all or nothing: one byte short of
/// the whole tail, a session crossed with its counters alone, and `drive
/// report` read the rows after its newest turn's mark as lost although they
/// would have fit many times over. Now the oldest rows go first, no more of
/// them than the room needs, and the differ's state only once no row is left.
///
/// RED before the fix: a room one byte short carried no row.
#[test]
fn a_room_short_of_the_whole_tail_carries_its_newest_rows() {
    let (term, turns) = session(150);
    let src = capture(&term, &turns, true);
    let turns_v: Vec<TurnRecord> = turns.lock().unwrap().records().cloned().collect();
    let mut archive = src.head.clone();
    assert!(
        term.lock()
            .unwrap()
            .alt_archive_carry_rows(&mut archive, src.fence, 0, usize::MAX)
    );
    let whole = Floors::default();
    let full = encode(&turns_v, whole, &archive).unwrap().len() as u64;
    let counters = encode(&turns_v, whole, &counters_only(archive.clone()))
        .unwrap()
        .len() as u64;
    for room in [full - 1, counters + (full - counters) / 2, counters + 64] {
        let (pick, shed) = encode_within(&turns_v, whole, archive.clone(), room).unwrap();
        assert_eq!(shed, 0, "room {room}: no record shed");
        assert!(pick.bytes.len() as u64 <= room, "room {room}: it fits");
        let c = decode(&pick.bytes).unwrap();
        assert_eq!(
            c.turns.len(),
            turns_v.len(),
            "room {room}: the whole ledger"
        );
        let a = c.archive.unwrap();
        assert!(
            !a.rows.is_empty() && a.rows.len() < archive.rows.len(),
            "room {room}: a room short of the whole tail carries its newest rows ({} of {})",
            a.rows.len(),
            archive.rows.len()
        );
        assert!(
            a.differ.is_some(),
            "room {room}: the differ's state goes only after every row"
        );
        // The newest rows: the tail's end, its older rows counted lost.
        let left_out = archive.rows.len() - a.rows.len();
        assert_eq!(a.last(), archive.last(), "room {room}");
        assert_eq!(a.rows[..], archive.rows[left_out..], "room {room}");
        assert_eq!(a.lost, archive.lost + left_out as u64, "room {room}");
        // As many as fit: one more row would not.
        let one_more = encode(
            &turns_v,
            whole,
            &newest_rows(archive.clone(), a.rows.len() + 1),
        )
        .unwrap()
        .len() as u64;
        assert!(one_more > room, "room {room}: one row more would not fit");
    }
}

/// A CROWDED DESK SHARES ITS ROWS ALIKE, EVERY SESSION'S NEWEST FIRST
/// (ATERM-KI-1015-2, U30): every session carries the rows after its newest
/// turn's mark (what `drive report` reads back), then as many of its older
/// rows as every other session, and keeps the differ's state, which is shed
/// only after the rows. Spent in pool order, the first sessions carried their
/// whole tails and every session after the one the rows ran out on crossed
/// with its counters alone, the same sessions on every update.
///
/// RED before the fix: sessions 28 to 63 carried no row (and with the rows
/// spent in pool order a few at a time, sessions 29 to 63).
#[test]
fn a_crowded_desk_shares_its_rows_alike_each_sessions_newest_first() {
    let (term, turns) = session_every(8000, 2000);
    let last = term.lock().unwrap().alt_archive().last();
    // The tail reaches back to the eighth newest submitted turn's mark, and
    // the newest turn's mark is 1500 rows from the end.
    let mark = |back: u64| ArchMark {
        origin: 7,
        last: last - back,
    };
    let newest = mark(1500);
    {
        let mut ledger = turns.lock().unwrap();
        *ledger = TurnLedger::default();
        for id in 1..=200 {
            let arch = match id {
                193 => mark(6000),
                200 => newest,
                _ => ArchMark::default(),
            };
            let mut r = record(id, true, arch);
            r.text = "x".repeat(480).into();
            ledger.push(r);
        }
    }
    let sources: Vec<CarrySource> = (0..64).map(|_| capture(&term, &turns, true)).collect();
    let alone = decode(&export(std::slice::from_ref(&sources[0]), ceiling())[0].1)
        .unwrap()
        .archive
        .unwrap();
    assert!(
        alone.differ.is_some() && alone.first == last - 6000 + 1 && alone.last() == last,
        "PRECONDITION: a session alone carries its whole tail and the differ's state"
    );
    let out = export(&sources, ceiling());
    assert_eq!(out.len(), 64, "every session carries a sidecar");
    let total: u64 = out.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(total <= MAX_AGGREGATE_BYTES, "{total}");
    let archives: Vec<AltArchiveCarry> = out
        .iter()
        .map(|(_, bytes)| decode(bytes).unwrap().archive.unwrap())
        .collect();
    let rows: Vec<usize> = archives.iter().map(|a| a.rows.len()).collect();
    let (fewest, most) = (*rows.iter().min().unwrap(), *rows.iter().max().unwrap());
    assert!(
        rows.iter().sum::<usize>() < 64 * alone.rows.len(),
        "PRECONDITION: the aggregate does not hold every whole tail"
    );
    for (i, a) in archives.iter().enumerate() {
        assert!(a.differ.is_some(), "session {i} keeps the differ's state");
        assert!(
            a.last() == last && a.first <= newest.last + 1,
            "session {i} carries the rows after its newest turn's mark ({} rows from {})",
            a.rows.len(),
            a.first
        );
        assert!(
            a.first < newest.last + 1,
            "session {i} carries older rows too"
        );
    }
    assert!(
        most - fewest <= most / 50,
        "every session carries about as many rows ({fewest} to {most})"
    );
    assert!(
        MAX_AGGREGATE_BYTES - total < 4096,
        "what the aggregate had left is spent ({} bytes unspent)",
        MAX_AGGREGATE_BYTES - total
    );
}

/// The codec keeps every carried field, unknown fields are skipped and
/// missing ones default, and an unknown version is dropped whole.
#[test]
fn the_sidecar_round_trips_and_tolerates_what_it_does_not_know() {
    let (term, turns) = session(60);
    let bytes = export(&[capture(&term, &turns, true)], ceiling())
        .remove(0)
        .1;
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
    assert!(
        [TurnBy::Harness, TurnBy::Ctl]
            .iter()
            .all(|by| want.iter().any(|t| t.by == *by))
            && want.iter().any(|t| matches!(t.by, TurnBy::Sid(_))),
        "the fixture carries every kind of caller"
    );

    // A newer sender's extra fields, and an older one's missing ones.
    let text = String::from_utf8(bytes.clone()).unwrap();
    // A record from a sender before `by=`, or naming a caller this build has
    // no word for, reads `-`, and the record itself is kept.
    for (from, to) in [(",\"by\":\"harness\"", ""), ("\"harness\"", "\"fleet\"")] {
        let older = decode(text.replacen(from, to, 1).as_bytes()).unwrap().turns;
        assert_eq!(older.len(), want.len(), "{from} -> {to}");
        let first = want.iter().position(|t| t.by == TurnBy::Harness).unwrap();
        assert_eq!(older[first].by, TurnBy::Unknown, "{from} -> {to}");
    }
    let extra = text.replacen('{', "{\"future_field\":[1,2,{\"x\":null}],", 1);
    assert_eq!(decode(extra.as_bytes()).unwrap().archive, carry.archive);
    let bare = decode(br#"{"version":1}"#).unwrap();
    assert!(bare.turns.is_empty() && bare.archive.is_none());
    assert!(
        decode(br#"{"version":2,"turns":[]}"#).is_none(),
        "unknown version"
    );
    assert!(decode(b"").is_none());

    // A status word this build does not print drops that record only, and the carry no longer
    // vouches for its id.
    let odd = text.replacen("\"settled\"", "\"exploded\"", 1);
    let odd = decode(odd.as_bytes()).unwrap();
    assert_eq!(odd.turns.len(), want.len() - 1);
    assert!(odd.unheld_below > 0, "the left-out id is not vouched for");
    // A turn whose caller hung up during its settle is carried like any other.
    let hung = text.replacen("\"settled\"", "\"hangup\"", 1);
    let hung = decode(hung.as_bytes()).unwrap().turns;
    assert_eq!(hung.len(), want.len());
    assert!(hung.iter().any(|t| t.status == "hangup"), "{hung:?}");
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
    let bytes = export(&[capture(&term, &turns, true)], ceiling())
        .remove(0)
        .1;
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
        let carry = decode(spoiled.as_bytes()).unwrap();
        let high = carry.high_turn_id().unwrap();
        assert!(high <= MAX_TURN_ID && high.checked_add(1).is_some());
        // A record left out is one the carry no longer vouches for, held to the range like
        // the sender's floor.
        let floor = if kept { 0 } else { MAX_TURN_ID + 1 };
        assert_eq!(carry.unheld_below, floor, "id {id}");
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

/// A record the receiver cannot read is left out and its id no longer vouched for, and a hop
/// to an older build spells every record in that build's words so none is left out.
#[test]
fn a_record_the_receiver_cannot_read_is_not_vouched_for() {
    let (term, turns) = session(30);
    {
        let mut ledger = turns.lock().unwrap();
        let mut records: Vec<TurnRecord> = ledger.records().cloned().collect();
        records.last_mut().expect("three records").status = "hangup";
        let mut hung = TurnLedger::default();
        for record in records {
            hung.push(record);
        }
        *ledger = hung;
    }
    let src = || capture(&term, &turns, true);
    let text =
        |words| String::from_utf8(export_for(&[src()], ceiling(), words).remove(0).1).unwrap();
    let current = text(StatusWords::Current);
    assert!(current.contains("\"status\":\"hangup\""), "{current}");

    // (a) This build's decoder, handed a word it does not know.
    let odd = current.replacen("\"status\":\"hangup\"", "\"status\":\"exploded\"", 1);
    let mut carry = decode(odd.as_bytes()).unwrap();
    assert_eq!(
        carry.turns.iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![1, 11],
        "the record it cannot read is left out, the rest come"
    );
    assert!(carry.unheld_below > 21, "{}", carry.unheld_below);
    let adopted = adopted_ledger(Some(&mut carry));
    assert!(adopted.low_id() > Some(20), "{:?}", adopted.low_id());
    assert_eq!(adopted.len(), 2, "the readable records are still held");

    // (b) The same ledger handed to an older build, read as v0.98.0 reads
    // it: a record with any word but `settled` or `timeout` is left out.
    let read_by_v0_98 = |text: &str| -> usize {
        let wire: Wire = aterm_json::from_slice(text.as_bytes()).unwrap();
        wire.turns
            .iter()
            .filter(|t| matches!(t.status.as_str(), "settled" | "timeout"))
            .count()
    };
    // RELEASES.ledger: v0.97.0 and v0.98.0 predate the word, v0.100.0 reads it.
    let (v0_97, v0_98, v0_100) = (1_790_585_188, 1_790_607_299, 1_790_807_622);
    assert_eq!(READS_HANGUP_FROM_BUILD, v0_100);
    for pre in [0, v0_97, v0_98, v0_100 - 1] {
        assert_eq!(
            StatusWords::for_target(pre),
            StatusWords::PreHangup,
            "{pre}"
        );
    }
    let to_older = text(StatusWords::for_target(v0_98));
    assert!(!to_older.contains("hangup"), "{to_older}");
    assert_eq!(read_by_v0_98(&to_older), 3, "every record crosses");
    assert_eq!(
        read_by_v0_98(&current),
        2,
        "negative control: in this build's own words the older build loses it"
    );
    // The submitted turn crosses as submitted, its text whole.
    let carried = decode(to_older.as_bytes()).unwrap();
    let last = carried.turns.last().unwrap();
    assert_eq!(
        (last.id, last.submitted, last.status),
        (21, true, "timeout")
    );
    assert_eq!(&*last.text, "turn 21");
    // (c) v0.100.0, every build after it, this build and a newer successor
    // read `hangup` and get it as it is: a rollback from this build to v0.100.0
    // carries the record exactly, as it did before.
    let running = crate::running_build_number();
    for target in [v0_100, v0_100 + 1, running, running + 1] {
        assert_eq!(
            StatusWords::for_target(target),
            StatusWords::Current,
            "{target}"
        );
    }
    let to_v0_100 = text(StatusWords::for_target(v0_100));
    assert_eq!(to_v0_100, current, "the record crosses in its own word");
    let read_by_v0_100 = |text: &str| -> usize {
        let wire: Wire = aterm_json::from_slice(text.as_bytes()).unwrap();
        wire.turns
            .iter()
            .filter(|t| matches!(t.status.as_str(), "settled" | "timeout" | "hangup"))
            .count()
    };
    assert_eq!(read_by_v0_100(&to_v0_100), 3);
    assert!(to_v0_100.contains("\"status\":\"hangup\""), "{to_v0_100}");
}

/// THE HARNESS'S `send` AND `key enter` (`status=pressed`) CROSS TO EVERY
/// SUCCESSOR. The word is newer than every release, so whatever build the
/// export is for the sidecar says `timeout` with `"pressed":true`: this
/// build reads the record back as `pressed`, `by=harness`, submitted, and a
/// build that knows only `settled`, `timeout` and `hangup` (every release to
/// date, which ignores a field it does not know) holds all three records,
/// none left out and no floor raised past it. NEGATIVE CONTROL: spelled in
/// its own word, the older build leaves the record out.
#[test]
fn a_pressed_record_crosses_to_a_build_that_never_heard_the_word() {
    use crate::turn_ledger::STATUS_PRESSED;
    let (term, turns) = session(30);
    {
        let mut ledger = turns.lock().unwrap();
        let mut records: Vec<TurnRecord> = ledger.records().cloned().collect();
        let last = records.last_mut().expect("three records");
        last.status = STATUS_PRESSED;
        last.by = TurnBy::Harness;
        let mut pressed = TurnLedger::default();
        for record in records {
            pressed.push(record);
        }
        *ledger = pressed;
    }
    let read_by_v0_104 = |text: &str| -> usize {
        let wire: Wire = aterm_json::from_slice(text.as_bytes()).unwrap();
        wire.turns
            .iter()
            .filter(|t| matches!(t.status.as_str(), "settled" | "timeout" | "hangup"))
            .count()
    };
    for words in [StatusWords::Current, StatusWords::PreHangup] {
        let text = String::from_utf8(
            export_for(&[capture(&term, &turns, true)], ceiling(), words)
                .remove(0)
                .1,
        )
        .unwrap();
        assert!(
            !text.contains("\"status\":\"pressed\""),
            "{words:?}: {text}"
        );
        assert_eq!(text.matches("\"pressed\":true").count(), 1, "{text}");
        assert_eq!(read_by_v0_104(&text), 3, "{words:?}: every record crosses");
        let carried = decode(text.as_bytes()).unwrap();
        assert_eq!(carried.turns.len(), 3);
        let last = carried.turns.last().unwrap();
        assert_eq!(
            (last.id, last.submitted, last.status, &last.by),
            (21, true, STATUS_PRESSED, &TurnBy::Harness),
            "{words:?}"
        );
        assert!(
            carried.turns[..2].iter().all(|t| t.status == "settled"),
            "only the pressed record reads pressed"
        );
        // NEGATIVE CONTROL: in its own word.
        let own_word = text.replacen("\"status\":\"timeout\"", "\"status\":\"pressed\"", 1);
        assert_ne!(own_word, text);
        assert_eq!(read_by_v0_104(&own_word), 2, "the older build loses it");
    }
}

/// A LEDGER THAT SHED THE HARNESS'S RECORDS FROM INSIDE ITS RING (its
/// share, `turn_ledger::HARNESS_SHARE`) carries that floor, `shed_below`:
/// the manager's turn the shedding kept crosses, and the adopted ledger's
/// low-water is the sender's, so a `since-turn=` resume across the handoff
/// below it is told it missed records, as it was before. NEGATIVE CONTROL:
/// a ledger that shed nothing carries no `shed_below` at all, so its
/// sidecar's bytes are what they were before the field.
#[test]
fn a_ledger_that_shed_harness_records_carries_its_low_water() {
    use crate::turn_ledger::STATUS_PRESSED;
    let (term, turns) = session(0);
    let (low, shed) = {
        let mut ledger = turns.lock().unwrap();
        ledger.push(TurnRecord {
            by: TurnBy::Ctl,
            ..record(1, true, ArchMark::default())
        });
        for id in 2..=2 * LEDGER_CAP as u64 {
            ledger.push(TurnRecord {
                by: TurnBy::Harness,
                status: STATUS_PRESSED,
                ..record(id, true, ArchMark::default())
            });
        }
        (ledger.low_id(), ledger.shed_below())
    };
    assert!(shed > 2, "the fixture sheds from inside: {shed}");
    let text = String::from_utf8(
        export(&[capture(&term, &turns, false)], ceiling())
            .remove(0)
            .1,
    )
    .unwrap();
    assert_eq!(text.matches(&format!("\"shed_below\":{shed}")).count(), 1);
    let mut carry = decode(text.as_bytes()).unwrap();
    let adopted = adopted_ledger(Some(&mut carry));
    assert_eq!(adopted.since(None).next().map(|r| r.id), Some(1), "kept");
    assert_eq!(adopted.len(), LEDGER_CAP);
    assert_eq!((adopted.low_id(), adopted.shed_below()), (low, shed));
    // NEGATIVE CONTROL: nothing shed, no field.
    let (term, turns) = session(30);
    let text = String::from_utf8(
        export(&[capture(&term, &turns, false)], ceiling())
            .remove(0)
            .1,
    )
    .unwrap();
    assert!(!text.contains("shed_below"), "{text}");
    assert_eq!(decode(text.as_bytes()).unwrap().shed_below, 0);
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
        shed_below: 0,
        turn_ids: 0,
        archive: None,
        log: None,
        log_mirror: None,
    };
    let ledger = adopted_ledger(Some(&mut with_records));
    assert_eq!(ledger.low_id(), Some(40), "a held record is the low-water");
}

/// A `turn` still waiting when the park exported records into the exiting process afterwards,
/// so the carry must not vouch for its id: a subscriber resuming from before it is told, one
/// resuming from it is not.
#[test]
fn a_turn_open_at_the_export_is_not_vouched_for_by_the_carry() {
    let ledger = Arc::new(Mutex::new(TurnLedger::default()));
    for id in 1..=3 {
        ledger
            .lock()
            .unwrap()
            .push(record(id, true, ArchMark::default()));
    }
    ledger.lock().unwrap().begin(4);
    let (term, _) = session(20);
    let bytes = export(&[capture(&term, &ledger, true)], ceiling())
        .remove(0)
        .1;
    // The late settle, into the ledger the successor will never read.
    let late = record(4, true, ArchMark::default());
    ledger.lock().unwrap().record(late, || unreachable!());
    let mut carry = decode(&bytes).unwrap();
    let mut adopted = adopted_ledger(Some(&mut carry));
    adopted.push(record(5, true, ArchMark::default()));
    let low = adopted.low_id().unwrap();
    assert!(3 + 1 < low, "anchor 3 is told of the gap (low {low})");
    assert!(4 + 1 >= low, "anchor 4 saw it, and is not told (low {low})");
}

/// A turn open at the export whose record lands under an id minted after it
/// (`TurnLedger::record` re-numbers a record that lands behind a later one) is as lost to the
/// successor as one under its own id, so the carry vouches for no id up to the manifest's
/// turn-id ceiling. Two shapes: a preempted turn below a newer record, and two open turns
/// that record in the other order.
#[test]
fn a_preempted_open_turn_renumbered_after_the_export_is_not_vouched_for() {
    let ceiling = crate::control::turn_ids_for_handoff();
    let (term, _) = session(20);
    let settled = |id| record(id, true, ArchMark::default());

    // P (10) wedged and was preempted by `lease release force`; R (11) took
    // the slot and recorded before the export.
    let ledger = Arc::new(Mutex::new(TurnLedger::default()));
    {
        let mut l = ledger.lock().unwrap();
        l.push(settled(5));
        l.begin(10);
        l.begin(11);
        assert_eq!(l.record(settled(11), || unreachable!()), 11);
    }
    let bytes = export(&[capture(&term, &ledger, true)], ceiling)
        .remove(0)
        .1;
    // P settles in the exiting process after the export: it is re-numbered,
    // as late as the ceiling lets that process mint.
    let late = ledger.lock().unwrap().record(settled(10), || ceiling);
    assert_eq!(late, ceiling);
    let low = adopted_ledger(Some(&mut decode(&bytes).unwrap()))
        .low_id()
        .unwrap();
    assert!(late < low, "turn {late} is vouched for (low {low})");
    assert!(11 + 1 < low, "anchor 11 is not told of turn {late}");

    // Two turns open at once, both past every record: the newer records
    // first, so the older one is re-numbered past it.
    let ledger = Arc::new(Mutex::new(TurnLedger::default()));
    {
        let mut l = ledger.lock().unwrap();
        l.push(settled(5));
        l.begin(12);
        l.begin(13);
    }
    let bytes = export(&[capture(&term, &ledger, true)], ceiling)
        .remove(0)
        .1;
    {
        let mut l = ledger.lock().unwrap();
        assert_eq!(l.record(settled(13), || unreachable!()), 13);
        assert_eq!(l.record(settled(12), || ceiling), ceiling);
    }
    let low = adopted_ledger(Some(&mut decode(&bytes).unwrap()))
        .low_id()
        .unwrap();
    assert!(ceiling < low, "turn {ceiling} is vouched for (low {low})");
}

/// When the ledgers alone overflow the aggregate it is shared out: every session keeps its
/// newest records (a short ledger all of them, wherever it stands in pool order), the long
/// ones are cut evenly by no more than needed, and each carry names the ids it shed.
#[test]
fn a_desk_whose_ledgers_overflow_the_aggregate_keeps_every_sessions_newest_turns() {
    let (term, _) = session(20);
    let ledger = |n: u64| {
        let mut l = TurnLedger::default();
        for id in 1..=n {
            let mut r = record(id, true, ArchMark::default());
            r.text = "x".repeat(480).into();
            l.push(r);
        }
        Arc::new(Mutex::new(l))
    };
    let full = ledger(LEDGER_CAP as u64);
    let short = ledger(3);
    let mut sources: Vec<CarrySource> = (0..64).map(|_| capture(&term, &full, true)).collect();
    sources.push(capture(&term, &short, true));
    let alone = export(std::slice::from_ref(&sources[0]), ceiling())
        .remove(0)
        .1
        .len() as u64;
    assert!(
        alone * 64 > MAX_AGGREGATE_BYTES,
        "the fixture's ledgers overflow the aggregate ({alone} a session)"
    );
    let out = export(&sources, ceiling());
    assert_eq!(out.len(), 65, "every session carries a sidecar");
    let total: u64 = out.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(total <= MAX_AGGREGATE_BYTES, "{total}");
    assert!(
        total > MAX_AGGREGATE_BYTES / 10 * 9,
        "the sessions shed no more than the aggregate needs: {total}"
    );
    let carries: Vec<ControlCarry> = out.iter().map(|(_, b)| decode(b).unwrap()).collect();
    let kept = carries[0].turns.len();
    assert!(kept < LEDGER_CAP, "the fixture sheds");
    for (i, carry) in carries[..64].iter().enumerate() {
        assert!(
            carry.turns.len().abs_diff(kept) <= 1,
            "session {i} is cut as the others, to a record"
        );
        let first = carry.turns.first().map(|t| t.id).unwrap();
        assert_eq!(carry.turns.last().map(|t| t.id), Some(LEDGER_CAP as u64));
        assert_eq!(carry.unheld_below, first, "session {i} names what it shed");
    }
    assert_eq!(carries[64].turns.len(), 3, "the short ledger comes whole");
    assert_eq!(carries[64].unheld_below, 0);
}

/// The aggregate goes to every session's ledger and counters first, and to archive rows only
/// with what is left.
#[test]
fn a_crowded_desk_keeps_every_sessions_ledger() {
    let (term, turns) = session_every(8000, 2000);
    // A long-driven worker's ledger: a couple of hundred turns of long text.
    {
        let mut ledger = turns.lock().unwrap();
        *ledger = TurnLedger::default();
        for id in 1..=200 {
            let mut r = record(id, true, ArchMark::default());
            r.text = "x".repeat(480).into();
            ledger.push(r);
        }
    }
    let sources: Vec<CarrySource> = (0..64).map(|_| capture(&term, &turns, true)).collect();
    let want = turns.lock().unwrap().len();
    let alone = export(std::slice::from_ref(&sources[0]), ceiling())
        .remove(0)
        .1
        .len() as u64;
    assert!(
        alone * 64 > MAX_AGGREGATE_BYTES,
        "the fixture overflows the aggregate ({alone} a session)"
    );
    let out = export(&sources, ceiling());
    assert_eq!(out.len(), 64, "every session carries a sidecar");
    let total: u64 = out.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(total <= MAX_AGGREGATE_BYTES, "{total}");
    for (i, (_, bytes)) in out.iter().enumerate() {
        let carry = decode(bytes).unwrap();
        assert_eq!(carry.turns.len(), want, "session {i} keeps its ledger");
        assert_eq!(carry.unheld_below, 0, "session {i} vouches for it whole");
    }
    assert!(
        !decode(&out[0].1).unwrap().archive.unwrap().rows.is_empty(),
        "what is left still carries rows"
    );
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

/// A mangled sidecar (bytes flipped, cut short, spliced, numbers swapped for huge ones)
/// decodes to something the adopting engine installs or to nothing, and the engine then takes
/// random frames at random sizes without a panic.
#[test]
fn a_mangled_sidecar_never_panics_the_adopting_engine() {
    let (term, turns) = session(90);
    let checkpoint = term.lock().unwrap().checkpoint_carry(0).unwrap();
    let good = export(&[capture(&term, &turns, true)], ceiling())
        .remove(0)
        .1;
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
    assert!(
        some > 20 && none > 20,
        "both outcomes exercised: {some}/{none}"
    );
}

// ---- the session log in the sidecar (phase 2A) ----------------------------

/// A timeline whose session log holds `n` person-burst marks after its open.
fn timeline_with_log(n: usize) -> Arc<Mutex<crate::session_timeline::SessionTimeline>> {
    let tl = Arc::new(Mutex::new(
        crate::session_timeline::SessionTimeline::default(),
    ));
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        g.slog_mut().open_session(None, None, now);
        for _ in 0..n {
            g.slog_mut().human_input(true, now);
        }
    }
    tl
}

/// [`capture`] with the session's timeline, as the park takes a handed
/// session's.
fn capture_with_log(
    term: &Arc<Mutex<Terminal>>,
    turns: &Arc<Mutex<TurnLedger>>,
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
) -> CarrySource {
    let guard = term.lock().unwrap();
    capture_head(3, &guard, term, turns, Some(timeline), true)
}

/// Each session's sidecar, `(local_id, JSON)`.
type Sidecars = Vec<(u64, Vec<u8>)>;

/// Export with logs, the losses collected.
fn export_logs(sources: &[CarrySource]) -> (Sidecars, Vec<(u64, CarryLoss)>) {
    let mut losses = Vec::new();
    let out = export_noting(
        sources,
        ceiling(),
        StatusWords::Current,
        true,
        |id, loss| {
            losses.push((id, loss));
        },
    );
    (out, losses)
}

/// THE LOG RIDES THE SIDECAR, and an older receiver reads past it: this
/// build reads the member back as the carry's lines, while a receiver built
/// before it (its `Wire` with no `log`, mirrored here) decodes the same
/// ledger and archive it decodes from a sidecar without the member.
#[test]
fn the_log_rides_the_sidecar_and_an_older_receiver_reads_past_it() {
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(default)]
    #[derive(Default)]
    struct OlderWire {
        version: u64,
        turns: Vec<TurnWire>,
        unheld_below: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        archive: Option<ArchiveWire>,
    }
    let (term, turns) = session(60);
    let tl = timeline_with_log(30);
    let (with, losses) = export_logs(&[capture_with_log(&term, &turns, &tl)]);
    assert!(losses.is_empty(), "{losses:?}");
    tl.lock().unwrap().slog_mut().lift_read_fence();
    let without = export(&[capture(&term, &turns, true)], ceiling());
    let carry = decode(&with[0].1).expect("this build reads it");
    let lines = carry.log.clone().expect("the log crossed");
    assert!(lines[0].starts_with("slog-carry v=1 "), "{:?}", &lines[..1]);
    assert!(lines.iter().any(|l| l.starts_with("host state v=1 ")));
    let bare = decode(&without[0].1).unwrap();
    assert_eq!(
        (carry.turns.clone(), carry.archive.clone()),
        (bare.turns, bare.archive)
    );
    let older: OlderWire = aterm_json::from_slice(&with[0].1).expect("an older build reads it");
    let older_bare: OlderWire = aterm_json::from_slice(&without[0].1).unwrap();
    assert_eq!(
        aterm_json::to_vec(&older).unwrap(),
        aterm_json::to_vec(&older_bare).unwrap(),
        "the same ledger and archive, the member skipped"
    );
}

/// THE MEMBER IS WRITTEN ONLY FOR A SUCCESSOR THAT READS IT: the export for
/// an older target (a rollback, a channel switch) is byte for byte the export
/// without any log, so that receiver's aggregate is spent as before. A
/// target at least as new carries it.
#[test]
fn an_export_for_an_older_target_is_byte_identical_to_one_without_a_log() {
    let (term, turns) = session(40);
    let tl = timeline_with_log(50);
    let running = crate::running_build_number();
    let sources = || vec![capture_with_log(&term, &turns, &tl)];
    let older = export_for_target(&sources(), ceiling(), running.saturating_sub(1));
    let none = export(&sources(), ceiling());
    assert_eq!(older, none, "byte for byte");
    assert!(decode(&older[0].1).unwrap().log.is_none());
    let newer = export_for_target(&sources(), ceiling(), running);
    assert!(
        decode(&newer[0].1).unwrap().log.is_some(),
        "a same-build successor reads it"
    );
    assert!(successor_reads_session_log(running + 1, running));
    assert!(!successor_reads_session_log(running - 1, running));
}

/// A BAD OR OVERSIZED LOG COSTS ONLY THE LOG: a member of the wrong type,
/// past a carry's line bound, or holding lines no log reads, leaves the
/// ledger and archive exactly as a log-less carry's; the adopting log then
/// starts over after `gap why=handoff`. And a sidecar from an older sender
/// (no member) adopts the slice-one way.
#[test]
fn a_bad_or_oversized_log_costs_only_the_log() {
    let (term, turns) = session(40);
    let bytes = export(&[capture(&term, &turns, true)], ceiling())
        .remove(0)
        .1;
    let bare = decode(&bytes).unwrap();
    assert!(bare.log.is_none(), "an older sender's sidecar has no log");
    let text = String::from_utf8(bytes).unwrap();
    let huge = format!(
        "[{}]",
        vec!["\"r 1 mark - bell src=engine conf=exact t=1\""; aterm_slog::CARRY_MAX_LINES + 1]
            .join(",")
    );
    for member in [
        "5".to_string(),
        "[1]".to_string(),
        "{}".to_string(),
        "\"x\"".to_string(),
        "null".to_string(),
        huge,
    ] {
        let spliced = text.replacen('{', &format!("{{\"log\":{member},"), 1);
        let c = decode(spliced.as_bytes())
            .unwrap_or_else(|| panic!("`{}` cost the sidecar", &member[..member.len().min(20)]));
        assert!(c.log.is_none(), "no log");
        assert_eq!(
            (c.turns, c.archive),
            (bare.turns.clone(), bare.archive.clone())
        );
    }
    // Lines that do not read as a carry: the log starts over, nothing else
    // is touched.
    let spliced = text.replacen('{', "{\"log\":[\"slog-carry v=1 junk\",\"r 9\"],", 1);
    let mut c = decode(spliced.as_bytes()).unwrap();
    assert_eq!(c.turns, bare.turns);
    let lines = c.take_log();
    assert!(lines.is_some());
    let mut log = crate::session_log::SessionLog::default();
    log.adopt(
        crate::session_log::LogAdopt {
            ids: crate::session_log::LogCarry::parse("1 3 3 3"),
            seq: 99,
            lines,
            keeper_ep: None,
            mirror: None,
        },
        false,
        None,
        aterm_slog::Stamp::new(1_760_000_000_000, 5),
    );
    let first = log.log().window().next().expect("a record");
    assert_eq!(first.k, aterm_slog::Kind::Gap, "the uncarried start");
    assert_eq!(first.seq, 100);
}

/// THE LOG NEVER COSTS THE TURN LEDGER: a timeline another thread holds
/// through the whole log pass costs its session's log (said, `LogBusy`), and
/// every session still carries its whole ledger and archive. CONTROL: free,
/// the log crosses.
#[test]
fn the_log_never_costs_the_turn_ledger() {
    let (term, turns) = session(40);
    let busy = timeline_with_log(10);
    let free = timeline_with_log(10);
    let sources = vec![
        capture_with_log(&term, &turns, &busy),
        capture_with_log(&term, &turns, &free),
    ];
    let held = busy.lock().unwrap();
    let (out, losses) = export_logs(&sources);
    drop(held);
    assert_eq!(
        losses,
        vec![(3, CarryLoss::LogBusy)],
        "said, for that session"
    );
    let want = turns.lock().unwrap().len();
    for (_, bytes) in &out {
        let c = decode(bytes).unwrap();
        assert_eq!(c.turns.len(), want, "the ledger crossed whole");
        assert!(
            !c.archive.unwrap().rows.is_empty(),
            "and the archive's rows"
        );
    }
    assert!(decode(&out[0].1).unwrap().log.is_none());
    assert!(
        decode(&out[1].1).unwrap().log.is_some(),
        "CONTROL: free, it crossed"
    );
}

/// LOG DETAIL IS SHED BEFORE ANY SESSION'S ARCHIVE ROWS: a desk whose rows
/// overflow the aggregate carries each log's skeleton (the open path, the
/// state) and no detail, while the desk keeps the rows it keeps without
/// logs, less only what the skeletons take. CONTROL: a single session, the
/// aggregate unpressed, carries its log's detail too.
#[test]
fn log_detail_is_shed_before_any_sessions_archive_rows() {
    let (term, turns) = session_every(8000, 2000);
    let tl = timeline_with_log(2000);
    let sources: Vec<CarrySource> = (0..64)
        .map(|_| capture_with_log(&term, &turns, &tl))
        .collect();
    let plain: Vec<CarrySource> = (0..64).map(|_| capture(&term, &turns, true)).collect();
    let without = export(&plain, ceiling());
    let total_without: u64 = without.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(
        total_without > MAX_AGGREGATE_BYTES - MAX_AGGREGATE_BYTES / 10,
        "PRECONDITION: the rows press the aggregate ({total_without})"
    );
    let (with, _) = export_logs(&sources);
    let total_with: u64 = with.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(total_with <= MAX_AGGREGATE_BYTES, "{total_with}");
    let rows = |b: &[u8]| decode(b).unwrap().archive.unwrap().rows.len();
    // The rows' bytes in a sidecar's JSON.
    let row_bytes = |b: &[u8]| -> u64 {
        let archive = decode(b).unwrap().archive.unwrap();
        archive.rows.iter().map(|r| json_string_len(r) + 1).sum()
    };
    let mut skeletons = 0_u64;
    for (i, (_, w)) in with.iter().enumerate() {
        let c = decode(w).unwrap();
        let lines = c.log.expect("the skeleton crossed");
        skeletons += log_json_len(&lines);
        let head = &lines[0];
        let carried: u64 = head
            .split(' ')
            .find_map(|t| t.strip_prefix("carried="))
            .and_then(|v| v.parse().ok())
            .unwrap();
        assert!(
            carried < 100,
            "session {i}: the detail is shed (only what the rows left): {head}"
        );
    }
    let rows_without: u64 = without.iter().map(|(_, p)| row_bytes(p)).sum();
    let rows_with: u64 = with.iter().map(|(_, w)| row_bytes(w)).sum();
    assert!(
        rows_with + skeletons + 4096 >= rows_without,
        "the rows the logs cost are what their skeletons take ({rows_with} + {skeletons} of \
         {rows_without})"
    );
    assert!(rows(&with[0].1) > 0, "the rows crossed");
    // CONTROL: one session alone carries its detail.
    tl.lock().unwrap().slog_mut().lift_read_fence();
    let (alone, _) = export_logs(&sources[..1]);
    let lines = decode(&alone[0].1).unwrap().log.unwrap();
    assert!(lines.len() > 1000, "the detail crossed: {}", lines.len());
}

/// THE LOG MEMBER IS SPLICED INTO THE SIDECAR ALREADY ENCODED: the export no
/// longer serializes a session's ledger and archive a second time to add its
/// log. The spliced sidecar reads back as the sidecar without it, member for
/// member, plus the log's lines exactly. NEGATIVE CONTROL: what is not a
/// compact object with a member in it takes no splice (the export then
/// encodes the sidecar whole).
#[test]
fn a_spliced_log_member_reads_as_the_sidecar_and_its_lines() {
    let (term, turns) = session(30);
    let plain = export(&[capture(&term, &turns, true)], ceiling());
    let tl = timeline_with_log(300);
    let lines = tl
        .lock()
        .unwrap()
        .slog_carry_source()
        .encode(aterm_slog::CarryBudget::SESSION)
        .expect("carried")
        .lines;
    let spliced = with_log_member(&plain[0].1, &lines).expect("a compact object");
    let mut back = decode(&spliced).expect("reads");
    assert_eq!(back.take_log(), Some(lines.clone()), "the lines exactly");
    assert_eq!(
        format!("{back:?}"),
        format!("{:?}", decode(&plain[0].1).expect("reads")),
        "and the rest as it was"
    );
    assert_eq!(
        log_json_len(&lines),
        aterm_json::to_vec(&lines).expect("json").len() as u64 + 7,
        "the member's size, counted, is the size written"
    );
    // NEGATIVE CONTROL.
    for not in [&b"{}"[..], b"[1,2]", b"", b"{\"version\":1"] {
        assert!(with_log_member(not, &lines).is_none(), "{not:?}");
    }
}

/// FIXER F6: a MIRRORED log's cursor and head ride its sidecar whether or
/// not its log does, so a successor whose `Log::from_carry` refuses the
/// carried log still owes the old space's tail as one exact `why=reset`
/// cover (ruling Q7) — not only one whose log did not cross.
#[test]
fn a_mirrored_logs_cursor_crosses_beside_its_log_and_a_refused_carry_still_owes_its_tail() {
    let (term, turns) = session(30);
    let tl = timeline_with_log(10);
    // Mirrored: a take engaged it (nothing landed yet).
    {
        let mut g = tl.lock().unwrap();
        let now = g.slog().now();
        let _ = g.slog_mut().mirror_log().bus_copy(1, now);
    }
    let (out, losses) = export_logs(&[capture_with_log(&term, &turns, &tl)]);
    assert!(losses.is_empty(), "{losses:?}");
    let mut carry = decode(&out[0].1).expect("reads");
    let lines = carry.take_log().expect("the log crossed");
    let prior = carry.take_log_mirror().expect("and its cursor beside it");
    assert_eq!(prior.landed.ms, 0);
    assert!(prior.ms_head > prior.landed.ms, "{prior:?}");
    // The successor cannot read the carried log: it starts over, owing the
    // old space's tail.
    let mut refused = lines.clone();
    refused.push("zz a line no build reads".to_string());
    let mut log = crate::session_log::SessionLog::default();
    log.adopt(
        crate::session_log::LogAdopt {
            ids: crate::session_log::LogCarry::parse("1 9 9 9"),
            seq: 1 << 20,
            lines: Some(refused),
            keeper_ep: None,
            mirror: Some(prior),
        },
        false,
        None,
        aterm_slog::Stamp::new(1_760_000_000_000, 5),
    );
    assert_eq!(
        log.log().mirror_prior(),
        Some(prior),
        "the old space is owed"
    );
}

/// THE BUS MIRROR'S OLD-SPACE CURSOR rides a sidecar whose log did not
/// cross (session log phase 4, ruling Q7), spliced in as the log member is:
/// it reads back as the sidecar without it plus the cursor exactly. An
/// unmirrored session's sidecar holds no byte of it.
#[test]
fn a_mirrored_log_that_does_not_cross_carries_its_cursor_in_the_sidecar() {
    let (term, turns) = session(30);
    let plain = export(&[capture(&term, &turns, true)], ceiling());
    assert!(
        !String::from_utf8_lossy(&plain[0].1).contains("log_mirror"),
        "an unmirrored sidecar is the bytes it was"
    );
    let prior = aterm_slog::Prior {
        landed: aterm_slog::BusCursor::new(2, 30, 7),
        ms_head: 9,
    };
    let spliced = with_mirror_member(&plain[0].1, prior).expect("a compact object");
    let mut back = decode(&spliced).expect("reads");
    assert_eq!(back.take_log_mirror(), Some(prior), "the cursor exactly");
    assert_eq!(
        format!("{back:?}"),
        format!("{:?}", decode(&plain[0].1).expect("reads")),
        "and the rest as it was"
    );
    assert_eq!(read_mirror_member("2:30:7:9"), Some(prior));
    for bad in ["2:30:7", "x:1:2:3", "", "1:2:3:4:5"] {
        assert_eq!(read_mirror_member(bad), None, "{bad:?}");
    }
}

/// M2 for the log member: a valid sidecar with a log, mangled (bytes
/// flipped, cut short, numbers swapped for huge ones), either decodes or is
/// dropped; when it decodes, its log is adopted or refused, never a panic,
/// and the adopted log's records stay in seq order.
#[test]
fn a_mangled_log_member_never_panics_the_adopting_log() {
    let (term, turns) = session(20);
    let tl = timeline_with_log(200);
    let (out, _) = export_logs(&[capture_with_log(&term, &turns, &tl)]);
    let good = out[0].1.clone();
    assert!(decode(&good).unwrap().log.is_some());
    let mut rng = Rng(0x5eed_1234);
    for round in 0..300 {
        let mut b = good.clone();
        match round % 3 {
            0 => {
                for _ in 0..1 + rng.below(4) {
                    let i = rng.below(b.len() as u64) as usize;
                    b[i] ^= 1 << rng.below(7);
                }
            }
            1 => b.truncate(rng.below(b.len() as u64) as usize),
            _ => {
                let text = String::from_utf8_lossy(&b).into_owned();
                let at = rng.below(text.len() as u64) as usize;
                let at = (0..=at)
                    .rev()
                    .find(|i| text.is_char_boundary(*i))
                    .unwrap_or(0);
                b = format!("{}99999999999999999999{}", &text[..at], &text[at..]).into_bytes();
            }
        }
        let Some(mut c) = decode(&b) else {
            continue;
        };
        let mut log = crate::session_log::SessionLog::default();
        log.adopt(
            crate::session_log::LogAdopt {
                ids: crate::session_log::LogCarry::parse("1 9 9 9"),
                seq: 1 << 20,
                lines: c.take_log(),
                keeper_ep: None,
                mirror: None,
            },
            false,
            None,
            aterm_slog::Stamp::new(1_760_000_000_000, 5),
        );
        let seqs: Vec<u64> = log.log().window().map(|r| r.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "round {round}");
    }
}

/// Mirror metadata shares the control carry's existing cap. Ordinary cores fill
/// that cap exactly; engaging the same logs must keep every session's newest
/// turns and its mirror cursor, shedding older turns before exceeding it.
#[test]
fn mirrored_cursors_are_reserved_before_the_control_carry_spends_its_budget() {
    const SESSIONS: usize = 16;
    let (term, _) = session(0);
    let archive = capture(&term, &Arc::new(Mutex::new(TurnLedger::default())), false).head;
    let share = MAX_AGGREGATE_BYTES / SESSIONS as u64;
    let mut records: Vec<TurnRecord> = (1..=LEDGER_CAP as u64)
        .map(|id| {
            let mut r = record(id, true, ArchMark::default());
            r.text = Arc::from("");
            r
        })
        .collect();
    let base = core_len(&records, Floors::default(), &archive);
    let mut left = usize::try_from(share.checked_sub(base).expect("room for text")).unwrap();
    // A C0 byte occupies six JSON bytes; ASCII occupies one. At most512
    // input bytes a turn, through the actual ledger and actual JSON encoder.
    for r in &mut records {
        let bytes = left.min(3000);
        let controls = bytes.saturating_sub(512).div_ceil(5);
        r.text = Arc::from("\u{1}".repeat(controls) + &"x".repeat(bytes - controls * 6));
        assert!(r.text.len() <= 512);
        left -= bytes;
    }
    assert_eq!(left, 0, "the bounded ledger can fill its share");
    let mut ledger = TurnLedger::default();
    for r in records {
        ledger.push(r);
    }
    let records: Vec<TurnRecord> = ledger.records().cloned().collect();
    assert_eq!(
        core_len(&records, Floors::default(), &archive),
        share,
        "exact encoded cores"
    );
    let ledger = Arc::new(Mutex::new(ledger));
    let timelines: Vec<_> = (0..SESSIONS).map(|_| timeline_with_log(1)).collect();
    let sources = || -> Vec<CarrySource> {
        timelines
            .iter()
            .enumerate()
            .map(|(i, tl)| {
                let mut source = capture_with_log(&term, &ledger, tl);
                source.local_id = i as u64 + 1;
                source
            })
            .collect()
    };
    let (ordinary, _) = export_logs(&sources());
    assert_eq!(ordinary.len(), SESSIONS);
    assert_eq!(
        ordinary.iter().map(|(_, b)| b.len() as u64).sum::<u64>(),
        MAX_AGGREGATE_BYTES,
        "CONTROL: ordinary cores spend the whole cap"
    );
    assert!(
        ordinary
            .iter()
            .all(|(_, b)| decode(b).unwrap().log_mirror.is_none())
    );
    for tl in &timelines {
        let mut g = tl.lock().unwrap();
        g.slog_mut().lift_read_fence();
        let now = g.slog().now();
        let _ = g.slog_mut().mirror_log().bus_copy(1, now);
        assert!(
            g.slog().log().mirror_landed().is_some(),
            "the actual take engaged it"
        );
    }
    let (mirrored, _) = export_logs(&sources());
    assert_eq!(mirrored.len(), SESSIONS, "every session retains a sidecar");
    for (_, bytes) in &mirrored {
        assert!(bytes.len() as u64 <= MAX_SIDECAR_BYTES);
        let c = decode(bytes).expect("the exported control carry reads");
        assert!(c.log_mirror.is_some(), "the cursor still crosses");
        assert_eq!(
            c.turns.last().map(|r| r.id),
            Some(LEDGER_CAP as u64),
            "every session keeps its newest turn"
        );
    }
    let total: u64 = mirrored.iter().map(|(_, b)| b.len() as u64).sum();
    assert!(
        total <= MAX_AGGREGATE_BYTES,
        "MIRROR_MEMBER_MUST_SHARE_THE_CONTROL_BUDGET: {total}"
    );
}

/// A ledger of the records `ids`, as a session holds one.
fn ledger_with(ids: impl IntoIterator<Item = u64>) -> TurnLedger {
    let mut ledger = TurnLedger::default();
    for id in ids {
        ledger.push(record(
            id,
            id % 2 == 0,
            ArchMark {
                origin: 7,
                last: id,
            },
        ));
    }
    ledger
}

/// THE CRASH CARRY (`crash_restore`'s increment 7) round-trips through the decoder an
/// update's sidecar goes through: every record (its text, caller, archive mark, words),
/// the floors and the writer's turn-id count, each record read as carried; no archive and
/// no log, whatever the bytes held.
#[test]
fn a_crash_carry_round_trips_the_ledger_alone() {
    let mut ledger = ledger_with(1..=20);
    ledger.begin(21);
    let carry = CrashLedger::of(&ledger, 30);
    assert_eq!(carry.len(), 20);
    let (bytes, shed) = carry.encode(|_| true).expect("fits");
    assert_eq!(shed, 0, "a ledger that fits is carried whole");
    let back = decode_crash_carry(&bytes).expect("decodes");
    assert_eq!(back.turns.len(), 20);
    for (got, id) in back.turns.iter().zip(1..=20) {
        let want = record(
            id,
            id % 2 == 0,
            ArchMark {
                origin: 7,
                last: id,
            },
        );
        assert_eq!(
            TurnRecord {
                carried: false,
                ..got.clone()
            },
            want,
            "record {id}"
        );
        assert!(got.carried);
    }
    assert_eq!(back.turn_ids, 30);
    assert_eq!(
        back.unheld_below, 22,
        "the export's floor: past the turn still open"
    );
    assert!(back.archive.is_none() && back.log.is_none() && back.log_mirror.is_none());

    // An update's sidecar decoded as a crash carry keeps its ledger and loses the rest: a
    // crash carry never installs an archive.
    let update = encode(
        &ledger.records().cloned().collect::<Vec<_>>(),
        Floors::default(),
        &archive_of(3),
    )
    .expect("an update's sidecar");
    assert!(decode_why(&update).expect("decodes").archive.is_some());
    let crash = decode_crash_carry(&update).expect("decodes");
    assert!(crash.archive.is_none());
    assert_eq!(crash.turns.len(), 20);
}

/// An update's sidecar names no turn-id count: its bytes are what they were before the
/// crash carry's field.
#[test]
fn an_update_sidecar_carries_no_turn_id_count() {
    let ledger = ledger_with(1..=3);
    let bytes = encode(
        &ledger.records().cloned().collect::<Vec<_>>(),
        Floors::default(),
        &archive_of(3),
    )
    .expect("encodes");
    let text = String::from_utf8(bytes.clone()).expect("JSON");
    assert!(!text.contains("turn_ids"), "{text}");
    assert_eq!(decode_why(&bytes).expect("decodes").turn_ids, 0);
}

/// A ledger too large for the slot sheds its OLDEST records, the fewest that let the rest
/// fit, and no longer vouches for their ids; when not even its floors fit, nothing.
#[test]
fn a_crash_carry_sheds_its_oldest_records_to_fit() {
    let ledger = ledger_with(1..=50);
    let carry = CrashLedger::of(&ledger, 50);
    // The size the newest ten take: exactly forty must be shed.
    let newest_ten = ledger_with(41..=50);
    let (ten, _) = CrashLedger::of(&newest_ten, 50)
        .encode(|_| true)
        .expect("fits");
    let limit = ten.len() + "turn".len();
    let (bytes, shed) = carry
        .encode(|bytes| bytes.len() <= limit)
        .expect("the newest fit");
    assert_eq!(shed, 40, "the count shed rides beside the bytes");
    assert!(bytes.len() <= limit);
    let back = decode_crash_carry(&bytes).expect("decodes");
    let ids: Vec<u64> = back.turns.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        (41..=50).collect::<Vec<_>>(),
        "the newest, as few shed as fit"
    );
    assert_eq!(back.unheld_below, 41, "no id it shed is vouched for");
    assert_eq!(back.turn_ids, 50);
    assert!(carry.encode(|_| false).is_none(), "nothing fits: no carry");
    // Room for the floors alone: every record shed, the floor past the newest.
    let floors_only = encode_ledger(
        &[],
        Floors {
            unheld_below: 4,
            shed_below: 0,
        },
        9,
    )
    .expect("encodes");
    let (bytes, shed) = CrashLedger::of(&ledger_with(1..=3), 9)
        .encode(|bytes| bytes.len() <= floors_only.len())
        .expect("the floors fit");
    assert_eq!((bytes, shed), (floors_only, 3));
}

/// An archive carry under `origin`, one row.
fn archive_of(origin: u64) -> AltArchiveCarry {
    AltArchiveCarry {
        origin,
        first: 1,
        rows: vec![Arc::from("a row")],
        ..AltArchiveCarry::default()
    }
}
