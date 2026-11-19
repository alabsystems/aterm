// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first format-2 write (tracker FL-148F, FL-D25): it waits on fourteen days of this Mac's
//! own fence, any seated member signs it, two first writers converge, and a Mac whose build
//! reads no format 2 writes as it always did.

use super::*;
use crate::client::{MemberEntry, MemberWritten, OnBusy, member_set, sync, two_log_macs_for_test};
use crate::commons::hearing::{FIRST_WRITE, GAP, Said, Version};
use crate::crossing::{FORMAT_ONE, FORMAT_TWO, Faults, inject};
use crate::membership_log::{RecordId, VERSION, VERSION_TWO};
use std::collections::BTreeSet;

/// 2030-03-17, a whole hour: the wall clock the Macs judge their fence by.
const T0: u64 = 1_900_000_800_000;
const HOUR: u64 = 60 * 60 * 1000;

/// A plain `set` of `name` to `value`.
fn entry(name: &str, value: &[u8]) -> MemberEntry {
    MemberEntry {
        name: name.to_owned(),
        value: crate::SecretBytes::from(value.to_vec()),
        if_absent: false,
        anchor: crate::client::Anchor::Keep,
        fleet_reads_anchors: false,
        read_as: None,
        settles: None,
    }
}

/// A person's patience: the hang detector, never a latency budget.
fn refuse() -> OnBusy {
    OnBusy::Refuse(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN)
}

/// From now on `paths`'s build reads format 2 and its clock reads `now`.
fn reads_two_at(paths: &Paths, now: u64) {
    inject(
        super::super::format_mark(paths).path(),
        Faults {
            follows: true,
            now_ms: Some(now),
            ..Faults::default()
        },
    );
}

/// From now on the build of `macs`'s vault server reads format 2: it follows a seat's signed
/// version-2 record as a Mac does, so the Macs read their newest records from it.
fn server_reads_two(
    macs: &crate::client::log_entries_tests::TwoMacs,
) -> crate::crossing::FormatMark {
    let mark = crate::crossing::FormatMark::at(
        macs.root
            .path()
            .join("s1")
            .join(format!("{}.format", crate::ALAB_VAULT)),
    );
    inject(
        mark.path(),
        Faults {
            follows: true,
            ..Faults::default()
        },
    );
    mark
}

/// `paths` hears every Commons member at `now` reporting `version`, and judges its fence.
fn judge(paths: &Paths, now: u64, version: Version) -> Option<(hearing::FenceWatch, u64)> {
    reads_two_at(paths, now);
    let bootstrap = Bootstrap::cached(paths).unwrap().expect("a Commons roster");
    let access = bootstrap.access().unwrap().expect("roster access");
    let mut ledger = Ledger::read(&bootstrap);
    for member in access.members() {
        ledger.record(member.binding().actor(), now, Said::Version(version));
    }
    ledger.write(&bootstrap).unwrap();
    observe(paths).unwrap()
}

/// `paths` judges its fence every `step` from `from` until `until`, every member on this build.
fn judge_until(paths: &Paths, from: u64, until: u64, step: u64) {
    let mut at = from;
    while at <= until {
        judge(paths, at, Version::own());
        at += step;
    }
}

/// The ids of the records `paths` holds.
fn ids(paths: &Paths) -> BTreeSet<RecordId> {
    super::super::load_records(paths)
        .unwrap()
        .log()
        .map(|log| log.records().map(|record| *record.id()).collect())
        .unwrap_or_default()
}

/// The version of each record `paths` holds that is not among `before`.
fn new_versions(paths: &Paths, before: &BTreeSet<RecordId>) -> Vec<u8> {
    let store = super::super::load_records(paths).unwrap();
    store
        .log()
        .unwrap()
        .records()
        .filter(|record| !before.contains(record.id()))
        .map(crate::membership_log::Record::version)
        .collect()
}

/// `paths` sets `name` to `value` as a seated member, and the versions it signed.
fn set(paths: &Paths, name: &str, value: &[u8]) -> Vec<u8> {
    let before = ids(paths);
    let written = member_set(paths, &entry(name, value), refuse()).unwrap();
    assert!(
        matches!(written, MemberWritten::Posted | MemberWritten::SignedHere),
        "{written:?}"
    );
    new_versions(paths, &before)
}

fn format_of(paths: &Paths) -> u32 {
    super::super::format_mark(paths).stored().unwrap()
}

/// Nothing signs format 2 before this Mac's fence held fourteen days without a break: not on the
/// day before, not fourteen days after a watch a gap of more than [`GAP`] broke, and never while
/// a member reports a release below the floor. Each write before then signs version 1 and
/// stores no format mark; once the floor held fourteen days, the next write signs version 2.
/// NEGATIVE CONTROL (observed): with the first write waiting on no fence
/// (`Faults::first_write_unfenced` set beside `follows` in `reads_two_at`), the first `set`
/// signs version 2 at `T0` and `assert_eq!(set(..), vec![VERSION])` fails with `left: [2]`.
#[test]
fn vault_checkpoint_nothing_writes_format_two_while_the_floor_has_not_held_fourteen_days() {
    let macs = two_log_macs_for_test(&[]);
    let m7 = &macs.m7;
    // A member below the floor holds the fence: no watch begins.
    assert_eq!(judge(m7, T0, Version(0, 1, 0)), None);
    reads_two_at(m7, T0);
    assert_eq!(set(m7, "a", b"1"), vec![VERSION]);
    // A day short of fourteen, judged every six hours.
    judge_until(m7, T0, T0 + FIRST_WRITE - 6 * HOUR, 6 * HOUR);
    assert_eq!(set(m7, "a", b"2"), vec![VERSION]);
    assert_eq!(format_of(m7), FORMAT_ONE);
    // A gap past GAP starts the watch again: fourteen days from the first judgment do not do.
    let after_gap = T0 + FIRST_WRITE - 6 * HOUR + GAP + HOUR;
    judge_until(m7, after_gap, T0 + FIRST_WRITE + 6 * HOUR, 6 * HOUR);
    assert_eq!(set(m7, "a", b"3"), vec![VERSION]);
    assert_eq!(format_of(m7), FORMAT_ONE);
    // A member below the floor at any judgment breaks it.
    judge_until(
        m7,
        T0 + FIRST_WRITE + 12 * HOUR,
        after_gap + FIRST_WRITE - 6 * HOUR,
        6 * HOUR,
    );
    assert_eq!(judge(m7, after_gap + FIRST_WRITE, Version(0, 1, 0)), None);
    assert_eq!(set(m7, "a", b"4"), vec![VERSION]);
    assert_eq!(format_of(m7), FORMAT_ONE);
    // Fourteen unbroken days from a fresh start: the first format-2 write.
    let start = after_gap + FIRST_WRITE + HOUR;
    judge_until(m7, start, start + FIRST_WRITE, 6 * HOUR);
    assert_eq!(set(m7, "a", b"5"), vec![VERSION_TWO]);
    assert_eq!(format_of(m7), FORMAT_TWO);
}

/// A seated Mac with no admin key (m7) writes the vault's first format-2 record once its fence
/// held fourteen days: its format mark is stored, its records hold the version-2 `Put`, it reads
/// its value back, and m22, whose build reads format 2, follows it on that signed record and
/// reads the value. NEGATIVE CONTROL (observed): with `format_two::writer` answering
/// `Writer::One` where it would answer `Writer::First`, m7's write signs version 1 and
/// `assert_eq!(set(..), vec![VERSION_TWO])` fails with `left: [1]`.
#[test]
fn vault_checkpoint_a_member_seat_with_no_admin_key_writes_the_first_format_two_record_once_its_fence_held()
 {
    let macs = two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let server = server_reads_two(&macs);
    assert!(matches!(crate::client::can_administer(m7), Ok(false)));
    judge_until(m7, T0, T0 + FIRST_WRITE, 6 * HOUR);
    assert_eq!(set(m7, "gh_access_token", b"first"), vec![VERSION_TWO]);
    assert_eq!(format_of(m7), FORMAT_TWO);
    assert_eq!(
        server.stored().unwrap(),
        FORMAT_TWO,
        "the server follows m7's post"
    );
    assert!(
        super::super::load_records(m7)
            .unwrap()
            .reading()
            .admits_two()
    );
    assert_eq!(
        crate::client::stored(m7, "gh_access_token").as_deref(),
        Some(b"first".as_slice()),
        "m7 reads its own value back"
    );
    reads_two_at(m22, T0 + FIRST_WRITE);
    sync(m22, None).expect("m22 syncs");
    assert_eq!(format_of(m22), FORMAT_TWO, "m22 follows m7's signed record");
    assert_eq!(
        crate::client::stored(m22, "gh_access_token").as_deref(),
        Some(b"first".as_slice())
    );
    // m7's next write is of format 2 too.
    assert_eq!(set(m7, "gh_access_token", b"second"), vec![VERSION_TWO]);
}

/// Two Macs whose fences both held write first, each before it holds the other's record: each
/// stores one format mark and signs its own version-2 records, the server takes both, and once
/// each has synced, each holds every record of the other's (none lost) and both read one value
/// of the name they both set. NEGATIVE CONTROL (observed): with the commit of a first write not
/// reading the store again at format 2 (`*current = super::load_records(paths)?;` cut from
/// `commit_written` in `log_entries.rs`), m7's commit writes its format-1 store under the mark
/// it just stored, and `set` panics on its `unwrap` with "this Mac's membership records are in
/// format 2, which this build of aterm does not read".
#[test]
fn vault_checkpoint_two_first_writers_on_two_macs_converge() {
    let macs = two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let server = server_reads_two(&macs);
    judge_until(m7, T0, T0 + FIRST_WRITE, 6 * HOUR);
    judge_until(m22, T0, T0 + FIRST_WRITE, 6 * HOUR);
    let before = ids(m22);
    assert_eq!(set(m7, "k", b"seven"), vec![VERSION_TWO]);
    assert_eq!(ids(m22), before, "m22 has not heard m7's write");
    assert_eq!(set(m22, "k", b"twenty-two"), vec![VERSION_TWO]);
    assert_eq!(server.stored().unwrap(), FORMAT_TWO);
    sync(m7, None).expect("m7 syncs");
    sync(m22, None).expect("m22 syncs");
    sync(m7, None).expect("m7 syncs again");
    for mac in [m7, m22] {
        assert_eq!(format_of(mac), FORMAT_TWO);
        assert_eq!(
            std::fs::read(mac.format_file()).unwrap(),
            b"aterm-secrets records format 2\n",
            "one mark"
        );
    }
    assert_eq!(ids(m7), ids(m22), "every record on both Macs");
    let value = crate::client::stored(m7, "k");
    assert!(value.is_some());
    assert_eq!(crate::client::stored(m22, "k"), value, "one value on both");
}

/// A Mac whose build reads no format 2, every running build, writes as it always did: its write
/// signs version 1, the records file is what it held with the record's bytes on its end, and no
/// format mark, fence file or server mark is stored. NEGATIVE CONTROL (observed): with
/// `format_two::attach` attaching the writer whatever `writer` answered (its `Writer::One`
/// return cut), the write drafts version 2 into a store that reads format 1, which does not
/// hold it, and `set`'s `matches!` panics on `Overtaken(None)`.
#[test]
fn vault_checkpoint_an_unmarked_macs_write_path_is_unchanged() {
    let macs = two_log_macs_for_test(&[]);
    let m7 = &macs.m7;
    let server = crate::crossing::FormatMark::at(
        macs.root
            .path()
            .join("s1")
            .join(format!("{}.format", crate::ALAB_VAULT)),
    );
    let file = || std::fs::read(m7.records_data_file().unwrap()).unwrap();
    let held = file();
    let before = ids(m7);
    assert_eq!(set(m7, "k", b"v"), vec![VERSION]);
    let store = super::super::load_records(m7).unwrap();
    let added: Vec<&crate::membership_log::Record> = store
        .log()
        .unwrap()
        .records()
        .filter(|record| !before.contains(record.id()))
        .collect();
    let mut expected = held;
    expected.extend(crate::membership_log::read_records(added.iter().copied()).unwrap());
    assert_eq!(
        file(),
        expected,
        "the record's bytes on the end of the file"
    );
    assert!(!m7.format_file().exists());
    let bootstrap = Bootstrap::cached(m7).unwrap().unwrap();
    assert!(!bootstrap.fence_file().exists());
    assert_eq!(server.stored().unwrap(), FORMAT_ONE);
    assert_eq!(store.reading(), crate::membership_log::Reading::ONE);
}

/// A fence this Mac cannot judge (its watch's file cannot be written: here a folder stands at
/// its name) is no judgment that it held: the write signs version 1, as on a build that judges
/// no fence, and no format mark is stored. NEGATIVE CONTROL (observed): with the judgment's
/// error failing the write (`Faults::fence_error_fails_write`), `member_set` fails and `set`
/// panics on its `unwrap` naming the fence file.
#[test]
fn vault_checkpoint_a_fence_that_cannot_be_judged_signs_version_one() {
    let macs = two_log_macs_for_test(&[]);
    let m7 = &macs.m7;
    judge(m7, T0, Version::own()).expect("the watch begins");
    let bootstrap = Bootstrap::cached(m7).unwrap().unwrap();
    let fence = bootstrap.fence_file();
    std::fs::remove_file(&fence).unwrap();
    std::fs::create_dir(&fence).unwrap();
    reads_two_at(m7, T0 + HOUR);
    assert!(observe(m7).is_err(), "the watch cannot be written");
    assert_eq!(set(m7, "k", b"v"), vec![VERSION]);
    assert_eq!(format_of(m7), FORMAT_ONE);
}

/// A break of the fence's watch is written to its file, durably, not unlinked: a judgment that
/// does not hold leaves the file in place, read as no watch, and the next judgment that holds
/// begins a watch at its own time, within [`GAP`] of the broken one's latest or not. NEGATIVE
/// CONTROL (observed): with the break unlinking the file (`hearing::UNLINK_BREAK`, the unflushed
/// unlink a power cut can take back), the file is gone and `assert!(fence.exists(), ..)`
/// fails.
#[test]
fn vault_checkpoint_a_fence_break_is_written_not_unlinked() {
    let macs = two_log_macs_for_test(&[]);
    let m7 = &macs.m7;
    judge_until(m7, T0, T0 + 6 * HOUR, 6 * HOUR);
    let fence = Bootstrap::cached(m7).unwrap().unwrap().fence_file();
    assert_eq!(judge(m7, T0 + 12 * HOUR, Version(0, 1, 0)), None);
    assert!(fence.exists(), "the break is on disk");
    let bootstrap = Bootstrap::cached(m7).unwrap().unwrap();
    assert_eq!(hearing::FenceWatch::read(&bootstrap), None);
    let (watch, _) = judge(m7, T0 + 13 * HOUR, Version::own()).expect("a new watch");
    assert_eq!(watch.since, T0 + 13 * HOUR, "the watch begins again");
}

/// Every store a marked Mac loads drafts as version 2, the stores of paths that draft outside
/// an author's (a revoke's plan, an admin's publish, a retirement's reseal) among them: the
/// load attaches this Mac's writer to a store that reads format 2, and to no other. NEGATIVE
/// CONTROL (observed): with `load_records` attaching no writer (its `write_as_read` call cut),
/// the marked Mac's store drafts for none and `assert!(.. .writes_two(), ..)` fails.
#[test]
fn vault_checkpoint_a_marked_macs_every_store_drafts_version_two() {
    let macs = two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let writes_two = |paths: &Paths| {
        super::super::load_records(paths)
            .unwrap()
            .log()
            .unwrap()
            .writes_two()
    };
    assert!(!writes_two(m7), "a store at format 1");
    judge_until(m7, T0, T0 + FIRST_WRITE, 6 * HOUR);
    assert_eq!(set(m7, "k", b"v"), vec![VERSION_TWO]);
    assert!(writes_two(m7), "the marked Mac's store");
    reads_two_at(m22, T0 + FIRST_WRITE);
    assert!(!writes_two(m22), "an unmarked Mac's store");
}
