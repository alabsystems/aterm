// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The listing's UNSERVABLE mark and reservations on fixed readings and records: no test reads
//! the live machine or ledger.

use std::collections::HashMap;
use std::path::PathBuf;

use aterm_capacity::floor::GIB;
use aterm_capacity::{Held, Need, Reading, Record, TreeUse, Volume, gib, reserved};
use aterm_json::Value;

use super::{NOT_READ, UNSERVABLE, json_of, lines};

const DATA: &str = "/System/Volumes/Data";

/// A second volume, 68 GiB above its 32 GiB floor.
const SCRATCH: &str = "/Volumes/scratch";

/// The live reading of 2026-10-27: [`DATA`] with 60.7 GiB free over its 32 GiB floor, so
/// `above` (28.7 GiB) above it.
fn data() -> (Reading, u64) {
    let mut r = Reading::of_gib(48, 40, 14);
    let v = Volume {
        mount: PathBuf::from(DATA),
        total: 862 * GIB,
        free: 607 * GIB / 10,
        floor: 32 * GIB,
        room: 16 * GIB,
    };
    let above = v.above_floor();
    r.volume = Some(v);
    (r, above)
}

/// [`SCRATCH`], read as a claim's own volume.
fn scratch() -> Volume {
    Volume {
        mount: PathBuf::from(SCRATCH),
        total: 500 * GIB,
        free: 100 * GIB,
        floor: 32 * GIB,
        room: 16 * GIB,
    }
}

/// A claim by `pid`, made at `start_ms`, for `disk` bytes on `on`, held when `held`.
fn claim(pid: u32, start_ms: u64, disk: u64, on: &str, held: bool) -> Record {
    Record {
        pid,
        start_ms,
        seq: 0,
        checkout: "/Users//me/aterm".to_owned(),
        need: Need::new(format!("verify {pid}")).disk(disk, on),
        held: held.then_some(Held {
            admitted_ms: start_ms,
            cores: 1,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// The report's lines with `volumes` read besides `r`'s and `trees` swept.
fn report(
    r: &Reading,
    volumes: &[Volume],
    claims: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> Vec<String> {
    lines(r, volumes, None, &Ok(claims.to_vec()), trees, 1_000)
}

/// Each claim's line of the report, in the ledger's order.
fn claim_lines(r: &Reading, volumes: &[Volume], claims: &[Record]) -> Vec<String> {
    let out = report(r, volumes, claims, None);
    out[out.len() - claims.len()..].to_vec()
}

/// The JSON report with `volumes` read besides `r`'s and `trees` swept, parsed.
fn json_report(
    r: &Reading,
    volumes: &[Volume],
    claims: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> Value {
    let text = json_of(r, volumes, None, &Ok(claims.to_vec()), trees, 1_000);
    aterm_json::from_str(&text).expect("the report is JSON")
}

/// The JSON report's `key` of each claim, in the ledger's order.
fn json_claims(report: &Value, key: &str) -> Vec<Value> {
    report
        .get("claims")
        .and_then(Value::as_array)
        .expect("claims")
        .iter()
        .map(|c| {
            c.get(key)
                .cloned()
                .unwrap_or_else(|| panic!("{key}: {c:?}"))
        })
        .collect()
}

/// Each claim's `(unservable, reserved_disk)` in the JSON report, in the ledger's order.
fn json_marks(
    r: &Reading,
    volumes: &[Volume],
    claims: &[Record],
    trees: Option<&HashMap<u32, TreeUse>>,
) -> Vec<(Value, Value)> {
    let report = json_report(r, volumes, claims, trees);
    json_claims(&report, "unservable")
        .into_iter()
        .zip(json_claims(&report, "reserved_disk"))
        .collect()
}

/// A judged claim's JSON marks: `unservable`, reserving `disk` bytes.
fn judged(unservable: bool, disk: u64) -> (Value, Value) {
    (Value::from(unservable), Value::from(disk))
}

/// Whether `line` carries the UNSERVABLE mark.
fn marked(line: &str) -> bool {
    line.contains(UNSERVABLE)
}

/// Whether `line` says its claim reserves `bytes`.
fn reserves(line: &str, bytes: u64) -> bool {
    line.contains(&format!("reserves {}", gib(bytes)))
}

/// The mark's boundary is the queue's own: a waiter needing exactly the room above the floor is
/// not marked and reserves its whole need; a byte more and it is marked and reserves nothing.
#[test]
fn the_mark_starts_one_byte_past_the_room() {
    let (r, above) = data();
    for (need, mark, reserved) in [(above, false, above), (above + 1, true, 0)] {
        let claims = [claim(58_518, 2, need, DATA, false)];
        let shown = claim_lines(&r, &[], &claims);
        assert_eq!(marked(&shown[0]), mark, "needing {need} B: {shown:?}");
        assert_eq!(
            json_marks(&r, &[], &claims, None),
            [judged(mark, reserved)],
            "needing {need} B"
        );
    }
}

/// A held claim is never marked, whatever its need: `--admit now` started one of 50 GiB on
/// 28.7 GiB of room, and it reserves all 50 against the claims behind it, on its line too. A
/// waiter is judged on its own volume, whichever the listing reads: on [`SCRATCH`] (68 GiB above
/// its floor) one of 500 GiB is marked and reserves 0 and one of 10 GiB reserves its need, and
/// that volume has its own disk line and its own entry of the JSON's `volumes`, with its free
/// space and floor (none where no other volume was read). Where a waiter's volume could not be
/// read it is not judged: no mark, and its JSON marks are `null`, never a `false` that reads as
/// fitting. Negative controls: judged on the listing's volume alone, the 500 GiB waiter is not
/// marked; with no `volumes` array, the JSON names no figure of [`SCRATCH`].
#[test]
fn each_claim_is_judged_on_its_own_volume() {
    let (r, _) = data();
    let claims = [
        claim(41_000, 1, 50 * GIB, DATA, true),
        claim(58_518, 2, 500 * GIB, SCRATCH, false),
        claim(89_756, 3, 10 * GIB, SCRATCH, false),
    ];
    let unread = claim_lines(&r, &[], &claims);
    assert!(!unread.iter().any(|l| marked(l)), "{unread:#?}");
    assert!(
        unread[1..].iter().all(|l| l.contains(NOT_READ)),
        "{unread:#?}"
    );
    assert_eq!(
        json_marks(&r, &[], &claims, None),
        [
            judged(false, 50 * GIB),
            (Value::Null, Value::Null),
            (Value::Null, Value::Null)
        ]
    );
    let read = [scratch()];
    let shown = claim_lines(&r, &read, &claims);
    assert_eq!(
        shown.iter().map(|l| marked(l)).collect::<Vec<_>>(),
        [false, true, false],
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|l| l.contains(NOT_READ)), "{shown:#?}");
    assert!(reserves(&shown[2], 10 * GIB), "{shown:#?}");
    assert_eq!(
        json_marks(&r, &read, &claims, None),
        [
            judged(false, 50 * GIB),
            judged(true, 0),
            judged(false, 10 * GIB)
        ]
    );
    let above = gib(scratch().above_floor());
    assert!(
        report(&r, &read, &claims, None)
            .iter()
            .any(|l| l.starts_with("disk") && l.contains(SCRATCH) && l.contains(&above)),
        "a disk line for {SCRATCH}"
    );
    let figures = |volumes: &[Volume]| -> Vec<(Value, Value, Value)> {
        json_report(&r, volumes, &claims, None)
            .get("volumes")
            .and_then(Value::as_array)
            .expect("volumes")
            .iter()
            .map(|v| {
                let key = |k: &str| v.get(k).cloned().unwrap_or_else(|| panic!("{k}: {v:?}"));
                (key("mount"), key("free"), key("floor"))
            })
            .collect()
    };
    assert_eq!(
        figures(&read),
        [(
            Value::from(SCRATCH),
            Value::from(100 * GIB),
            Value::from(32 * GIB)
        )]
    );
    assert!(figures(&[]).is_empty(), "no other volume read");
    assert!(reserves(&shown[0], 50 * GIB), "{shown:#?}");
}

/// What a claim reserves is its need less what is written of it, never the need itself: a held
/// claim of 20 GiB whose holder's tree wrote 5 reserves 15, on its line as in its JSON; a waiter
/// of 6 GiB whose own count says 2 written reserves 4, in its JSON and on its line; an
/// unservable waiter reserves 0 whatever it wrote. A held VM boot's 16 GiB of memory, its
/// holder's tree using 2 GiB and the VM process credited to it 6, reserves the 8 GiB
/// [`reserved`] reckons, its JSON's `reserved_mem` and its line's `mem 16 GiB (…), reserves
/// 8 GiB`; a claim of no memory reserves none. Only the unservable waiter's line carries the
/// mark. Negative control: with the held line's disk reservation guarded to waiters again, the
/// 20 GiB claim's line names no reservation.
#[test]
fn a_reservation_is_the_need_less_what_is_written() {
    let (r, _) = data();
    let mut wrote = claim(89_756, 3, 6 * GIB, DATA, false);
    wrote.wrote = Some(2 * GIB);
    let mut far = claim(58_518, 2, 565 * GIB / 10, DATA, false);
    far.wrote = Some(GIB);
    let mut vm = claim(52_000, 4, 0, DATA, true);
    vm.need = Need::new("lab windows exec").mem(16 * GIB);
    vm.credit = vec![52_100];
    let claims = [claim(41_000, 1, 20 * GIB, DATA, true), far, wrote, vm];
    let use_of = |footprint: u64, written: u64| TreeUse { footprint, written };
    let trees = HashMap::from([
        (41_000, use_of(0, 5 * GIB)),
        (52_000, use_of(2 * GIB, 0)),
        (52_100, use_of(6 * GIB, 0)),
    ]);
    assert_eq!(
        json_marks(&r, &[], &claims, Some(&trees)),
        [
            judged(false, 15 * GIB),
            judged(true, 0),
            judged(false, 4 * GIB),
            (Value::from(false), Value::Null)
        ]
    );
    let out = report(&r, &[], &claims, Some(&trees));
    let shown = &out[out.len() - claims.len()..];
    assert!(reserves(&shown[0], 15 * GIB), "{shown:#?}");
    assert!(reserves(&shown[2], 4 * GIB), "{shown:#?}");
    assert_eq!(
        shown.iter().map(|l| marked(l)).collect::<Vec<_>>(),
        [false, true, false, false],
        "{shown:#?}"
    );
    assert!(reserves(&shown[1], 0), "{shown:#?}");
    let reckoned = reserved(&claims, Some(&trees), r.volume.as_slice());
    assert_eq!(reckoned[3].mem, 8 * GIB, "{reckoned:?}");
    assert_eq!(
        json_claims(&json_report(&r, &[], &claims, Some(&trees)), "reserved_mem"),
        reckoned
            .iter()
            .map(|c| Value::from(c.mem))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        reckoned[..3].iter().map(|c| c.mem).collect::<Vec<_>>(),
        [0, 0, 0],
        "no memory claimed, none reserved"
    );
    assert!(
        shown[3].contains(&format!("mem {}", gib(16 * GIB)))
            && shown[3].contains(&format!(", reserves {}", gib(8 * GIB))),
        "{shown:#?}"
    );
}
