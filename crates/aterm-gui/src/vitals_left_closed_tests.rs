// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed-tab record over a scratch state home: what reads as none, the bounds, and two
//! writers of one record.

use super::*;

const BOOT: &str = "b";
/// Now, unix ms.
const NOW: u64 = 1_792_000_000_000;
const DAY_MS: u64 = 24 * 3_600_000;

/// A session id of the shape aterm mints, its last digits `n`.
fn sid(n: u64) -> String {
    format!("s-{n:020x}")
}

/// Tab `n`'s close at `closed_ms`: its shell (pid `n`, unique id `1000 + n`) and a horizon 500 on.
fn kept(n: u64, closed_ms: u64) -> Kept {
    Kept {
        tab: ClosedTab {
            key: n,
            sid: sid(n),
            label: format!("Tab {n}"),
            number: Some(2),
            shell_uniqueid: 1_000 + n,
            horizon: 1_500 + n,
            closed_ms,
            agent_spans: vec![AgentSpan {
                since: 1_200 + n,
                until: None,
            }],
            ..ClosedTab::default()
        },
        shell_pid: u32::try_from(n).unwrap(),
        at_quit: false,
        told_here: false,
    }
}

fn scratch() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("a scratch state home");
    let path = dir.path().join(FILE);
    (dir, path)
}

fn sids(kept: &[Kept]) -> Vec<String> {
    kept.iter().map(|k| k.tab.sid.clone()).collect()
}

/// What a writer kept reads back whole, restored and with no key yet: the window, the words and
/// the agent's stretches the judge reads, and a close its host told of reads told, while one a
/// reading counted a process left in that no host told of does not. NEGATIVE CONTROL: write only
/// a close's own told, and the one its host told of reads untold.
#[test]
fn a_written_close_reads_back_restored() {
    let (_dir, path) = scratch();
    let mut counted = kept(5, NOW - 2_000);
    counted.tab.left = 2;
    let mut told = kept(7, NOW - 1_000);
    told.told_here = true;
    write(&path, BOOT, [&counted, &told], NOW).expect("written");
    let read = read(&path, BOOT, NOW);
    let want = |mut k: Kept, was_told: bool| {
        k.tab.key = 0;
        k.tab.left = 0;
        k.tab.restored = true;
        k.tab.told = was_told;
        k.told_here = false;
        k
    };
    assert_eq!(
        read,
        [want(counted, false), want(told, true)],
        "a close its host told of is told"
    );
}

/// A FILE THAT DOES NOT READ WHOLE READS AS NONE: missing, not JSON, of another schema, or holding
/// one record no writer makes beside a good one; a write over such a file puts the table in its
/// place. NEGATIVE CONTROL: take the records that validate and skip the others, and the good one
/// reads.
#[test]
fn a_corrupt_or_missing_file_reads_as_none() {
    let (_dir, path) = scratch();
    assert!(read(&path, BOOT, NOW).is_empty(), "missing");
    std::fs::write(&path, "{\"schema\":1,\"boot\":\"b\",\"tabs\":[").unwrap();
    assert!(read(&path, BOOT, NOW).is_empty(), "torn");
    write(&path, BOOT, [&kept(5, NOW - 1_000)], NOW).expect("written over the torn file");
    assert_eq!(sids(&read(&path, BOOT, NOW)), [sid(5)]);
    let good = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, good.replace("\"schema\":1", "\"schema\":2")).unwrap();
    assert!(read(&path, BOOT, NOW).is_empty(), "another schema");
    let mut bad = kept(6, NOW - 500);
    bad.tab.horizon = bad.tab.shell_uniqueid - 1;
    std::fs::write(&path, &good).unwrap();
    let mixed = good.replace(
        "]}",
        &format!(
            ",{}]}}",
            aterm_json::to_string(&Rec::of(&bad)).expect("a record")
        ),
    );
    std::fs::write(&path, mixed).unwrap();
    assert!(
        read(&path, BOOT, NOW).is_empty(),
        "a window that closes before it opens spoils the file"
    );
    let mut bad_sid = kept(6, NOW - 500);
    bad_sid.tab.sid = "s-6".into();
    std::fs::write(
        &path,
        good.replace(
            "]}",
            &format!(
                ",{}]}}",
                aterm_json::to_string(&Rec::of(&bad_sid)).expect("a record")
            ),
        ),
    )
    .unwrap();
    assert!(
        read(&path, BOOT, NOW).is_empty(),
        "a session id aterm never mints spoils the file"
    );
}

/// A RECORD PAST THE BOUND IS DROPPED: one closed more than 30 days before the read is not read,
/// and of more than 512 the oldest closes go first; a write drops them too. NEGATIVE CONTROL:
/// keep every close whatever its age, and the 31-day-old one reads.
#[test]
fn a_record_past_the_bound_is_dropped() {
    let (_dir, path) = scratch();
    write(
        &path,
        BOOT,
        [&kept(5, NOW - 31 * DAY_MS), &kept(6, NOW - DAY_MS)],
        NOW - 2 * DAY_MS,
    )
    .expect("written");
    assert_eq!(
        sids(&read(&path, BOOT, NOW - 2 * DAY_MS)),
        [sid(5), sid(6)],
        "both inside 30 days of the write"
    );
    assert_eq!(sids(&read(&path, BOOT, NOW)), [sid(6)], "31 days old");
    std::fs::remove_file(&path).expect("a fresh record");
    let many: Vec<Kept> = (1..=u64::try_from(CLOSED_KEPT).unwrap() + 1)
        .map(|n| kept(n, NOW - 10_000 + n))
        .collect();
    write(&path, BOOT, &many, NOW).expect("written");
    let read = read(&path, BOOT, NOW);
    assert_eq!(read.len(), CLOSED_KEPT);
    assert_eq!(read[0].tab.sid, sid(2), "the oldest close went first");
    assert!(!sids(&read).contains(&sid(1)));
}

/// TWO WRITERS OF ONE RECORD KEEP EACH OTHER'S CLOSES: an aterm that never held the other's close
/// writes it back, and where both hold one close (one aterm restored it and told of it while the
/// one that recorded it writes it again, untold) one record stands, told where either says so.
/// NEGATIVE CONTROL: write the table without reading the record first, and the other's close is
/// lost.
#[test]
fn two_writers_keep_each_others_closes() {
    let (_dir, path) = scratch();
    write(
        &path,
        BOOT,
        [&kept(4, NOW - 3_000), &kept(5, NOW - 2_000)],
        NOW,
    )
    .expect("the first's");
    let mut told = kept(5, NOW - 2_000);
    told.told_here = true;
    write(&path, BOOT, [&told, &kept(6, NOW - 1_500)], NOW).expect("the second's");
    write(&path, BOOT, [&kept(5, NOW - 2_000)], NOW).expect("the first's again");
    let read = read(&path, BOOT, NOW);
    assert_eq!(
        sids(&read),
        [sid(4), sid(5), sid(6)],
        "each close once, in the order first written"
    );
    assert!(read[1].tab.told, "the telling stands");
}

/// `good`, a record file a writer made, with `rec` after its records.
fn with_rec(good: &str, rec: &Rec) -> String {
    good.replace(
        "]}",
        &format!(",{}]}}", aterm_json::to_string(rec).expect("a record")),
    )
}

/// A WRITER WRITES ONLY WHAT ITS READER TAKES: beside a good close, the table holds one whose
/// window closes before it opens (no process newer than its shell outlived it, so the reading
/// that missed it read a lower horizon) and one whose session id was never read; the record keeps
/// the good one, which reads. NEGATIVE CONTROL: write every close the table holds, and the record
/// reads as none.
#[test]
fn a_writer_writes_only_what_its_reader_takes() {
    let (_dir, path) = scratch();
    let mut empty = kept(6, NOW - 1_500);
    empty.tab.horizon = empty.tab.shell_uniqueid - 1;
    let mut unnamed = kept(8, NOW - 1_000);
    unnamed.tab.sid = String::new();
    write(&path, BOOT, [&kept(5, NOW - 2_000), &empty, &unnamed], NOW).expect("written");
    assert_eq!(
        sids(&read(&path, BOOT, NOW)),
        [sid(5)],
        "the good close reads"
    );
}

/// STRETCHES NO WRITER MAKES SPOIL THE FILE: a writer keeps a tab's agent stretches oldest first,
/// each closed before the next opens and only the last open, so a record whose stretches reopen an
/// earlier one, overlap or run out of order reads as none, and one of a writer's own shape reads.
/// NEGATIVE CONTROL: check each stretch alone, and the reopened one reads.
#[test]
fn stretches_no_writer_makes_spoil_the_file() {
    let (_dir, path) = scratch();
    write(&path, BOOT, [&kept(5, NOW - 2_000)], NOW).expect("written");
    let good = std::fs::read_to_string(&path).unwrap();
    let spans = |spans: &[(u64, Option<u64>)]| {
        let mut rec = Rec::of(&kept(6, NOW - 1_000));
        rec.agent_spans = spans.to_vec();
        rec
    };
    std::fs::write(
        &path,
        with_rec(&good, &spans(&[(1_200, Some(1_400)), (1_800, None)])),
    )
    .unwrap();
    assert_eq!(
        sids(&read(&path, BOOT, NOW)),
        [sid(5), sid(6)],
        "a writer's own shape"
    );
    for (bad, why) in [
        (
            &[(1_200, None), (1_800, None)][..],
            "an earlier stretch reopened",
        ),
        (&[(1_200, Some(1_900)), (1_800, None)][..], "overlapping"),
        (
            &[(1_800, Some(1_900)), (1_200, Some(1_400))][..],
            "out of order",
        ),
    ] {
        std::fs::write(&path, with_rec(&good, &spans(bad))).unwrap();
        assert!(read(&path, BOOT, NOW).is_empty(), "{why}");
    }
}

/// THE CLOSE WRITTEN FIRST GOES FIRST, whatever the clock says, as the pass drops its oldest close
/// first: with the record full, the clock is set back an hour and a close is written, dated before
/// every other; the record keeps it and drops the close written first. NEGATIVE CONTROL: order
/// the record by the close's time, and the new close is the one dropped.
#[test]
fn the_close_written_first_goes_first() {
    let (_dir, path) = scratch();
    let full = u64::try_from(CLOSED_KEPT).unwrap();
    let many: Vec<Kept> = (1..=full).map(|n| kept(n, NOW - 10_000 + n)).collect();
    write(&path, BOOT, &many, NOW).expect("written");
    let back = NOW - 3_600_000;
    write(&path, BOOT, [&kept(full + 1, back)], back + 1).expect("written with the clock back");
    let read = read(&path, BOOT, back + 1);
    assert_eq!(read.len(), CLOSED_KEPT);
    assert_eq!(
        read.last().map(|k| k.tab.sid.clone()),
        Some(sid(full + 1)),
        "the close written last is kept"
    );
    assert_eq!(read[0].tab.sid, sid(2), "the close written first went");
}

/// A WRITER THAT FINDS THE LOCK HELD WRITES NOTHING: while another aterm holds the record's lock
/// past the wait, a write is refused as busy and the record is as it was; once the lock is free,
/// the same write lands. NEGATIVE CONTROL: write without taking the lock, and the refused write
/// lands.
#[test]
fn a_held_lock_leaves_the_record_as_it_was() {
    let (_dir, path) = scratch();
    write(&path, BOOT, [&kept(5, NOW - 2_000)], NOW).expect("written");
    let lock = crate::vitals_host::open_lock_file(&path.with_file_name(LOCK)).expect("the lock");
    let held = crate::native_document_host::HeldAdvisoryLock::try_lock(lock)
        .expect("held by the other aterm");
    let refused = write(&path, BOOT, [&kept(6, NOW - 1_000)], NOW).map_err(|e| e.kind());
    assert_eq!(refused, Err(std::io::ErrorKind::WouldBlock), "busy");
    assert_eq!(sids(&read(&path, BOOT, NOW)), [sid(5)], "as it was");
    drop(held);
    write(&path, BOOT, [&kept(6, NOW - 1_000)], NOW).expect("written once the lock is free");
    assert_eq!(sids(&read(&path, BOOT, NOW)), [sid(5), sid(6)]);
}

/// A FILE PAST ITS BOUNDS READS AS NONE, though whole: larger than 2 MiB, more records than a
/// writer keeps, or a label longer than a writer keeps; one just within each reads. A writer cuts
/// a long label at a character's boundary. NEGATIVE CONTROL: read a file of any size, and the
/// padded one reads.
#[test]
fn a_file_past_its_bounds_reads_as_none() {
    let (_dir, path) = scratch();
    write(&path, BOOT, [&kept(5, NOW - 1_000)], NOW).expect("written");
    let good = std::fs::read_to_string(&path).unwrap();
    let room = usize::try_from(MAX_BYTES).unwrap() - good.len();
    std::fs::write(&path, format!("{good}{}", " ".repeat(room))).unwrap();
    assert_eq!(sids(&read(&path, BOOT, NOW)), [sid(5)], "2 MiB");
    std::fs::write(&path, format!("{good}{}", " ".repeat(room + 1))).unwrap();
    assert!(read(&path, BOOT, NOW).is_empty(), "a byte past 2 MiB");

    let doc = |tabs: Vec<Rec>| {
        aterm_json::to_string(&Doc {
            schema: SCHEMA,
            boot: BOOT.to_owned(),
            tabs,
        })
        .expect("a file")
    };
    let full = u64::try_from(CLOSED_KEPT).unwrap();
    let recs: Vec<Rec> = (1..=full + 1)
        .map(|n| Rec::of(&kept(n, NOW - 10_000 + n)))
        .collect();
    std::fs::write(&path, doc(recs[..CLOSED_KEPT].to_vec())).unwrap();
    assert_eq!(
        read(&path, BOOT, NOW).len(),
        CLOSED_KEPT,
        "as many as a writer keeps"
    );
    std::fs::write(&path, doc(recs)).unwrap();
    assert!(
        read(&path, BOOT, NOW).is_empty(),
        "one more than a writer keeps"
    );

    let mut long = Rec::of(&kept(5, NOW - 1_000));
    long.label = "x".repeat(LABEL_BYTES);
    std::fs::write(&path, doc(vec![long.clone()])).unwrap();
    assert_eq!(
        sids(&read(&path, BOOT, NOW)),
        [sid(5)],
        "a label a writer keeps"
    );
    long.label.push('x');
    std::fs::write(&path, doc(vec![long])).unwrap();
    assert!(
        read(&path, BOOT, NOW).is_empty(),
        "a label longer than a writer keeps"
    );

    let mut wide = kept(5, NOW - 1_000);
    wide.tab.label = "\u{20ac}".repeat(100);
    write(&path, BOOT, [&wide], NOW).expect("written");
    assert_eq!(
        read(&path, BOOT, NOW)[0].tab.label,
        "\u{20ac}".repeat(LABEL_BYTES / 3),
        "cut at the last whole character within the bound"
    );
}
