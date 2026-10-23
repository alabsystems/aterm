// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Coalition decoder tests on recorded records and the live macOS kernel.

use super::*;

/// The probes' mds coalition.
fn mds_record() -> [u64; USAGE_WORDS] {
    let mut raw: [u64; USAGE_WORDS] = std::array::from_fn(|i| 0xA5A5_0000 + i as u64);
    raw[0] = 1;
    raw[1] = 0;
    raw[3] = 100_755_763_200;
    raw[6] = 13_640_070_000;
    raw[23] = 7;
    raw[24..31].copy_from_slice(&[
        129_600_000,
        22_982_400_000,
        43_891_200_000,
        914_400_000,
        33_338_400_000,
        192_000_000,
        0,
    ]);
    raw[40] = 119_589_683;
    raw
}

#[test]
fn a_recorded_usage_record_decodes_its_words_and_scales_its_ticks() {
    let usage = decode_usage(307, &mds_record(), (125, 3)).expect("7 QoS classes");
    assert_eq!(
        usage,
        CoalitionUsage {
            id: 307,
            tasks_started: 1,
            tasks_exited: 0,
            cpu_ns: 4_198_156_800_000,
            bytes_read: 13_640_070_000,
            eqos_ns: [
                5_400_000_000,
                957_600_000_000,
                1_828_800_000_000,
                38_100_000_000,
                1_389_100_000_000,
                8_000_000_000,
                0,
            ],
            footprint: Some(119_589_683),
        }
    );
    let intel = decode_usage(307, &mds_record(), (1, 1)).expect("7 QoS classes");
    assert_eq!(intel.cpu_ns, 100_755_763_200);
    assert_eq!(intel.eqos_ns[2], 43_891_200_000);
}

/// A kernel whose record ends before `phys_footprint` leaves that word as
/// the buffer held it: the footprint is not given, never 0, and the rest
/// reads.
#[test]
fn a_record_cut_before_the_footprint_gives_none_for_it() {
    let mut raw = mds_record();
    raw[40..].fill(UNWRITTEN);
    let usage = decode_usage(307, &raw, (125, 3)).expect("7 QoS classes");
    assert_eq!(usage.footprint, None);
    assert_eq!(usage.cpu_ns, 4_198_156_800_000);
}

/// The negative control: a record whose QoS count is not this layout's, a
/// zeroed one, or one the kernel wrote nothing to, decodes to nothing.
#[test]
fn a_record_whose_qos_count_is_not_seven_decodes_to_none() {
    let mut raw = mds_record();
    raw[23] = 6;
    assert_eq!(decode_usage(307, &raw, (125, 3)), None);
    assert_eq!(decode_usage(2, &[0; USAGE_WORDS], (125, 3)), None);
    assert_eq!(decode_usage(2, &[UNWRITTEN; USAGE_WORDS], (125, 3)), None);
}

/// A listed coalition that answers nothing (coalition 2 answers `EINVAL` on
/// every read the probes made) or whose record does not decode is dropped,
/// never a zero row; the count and the newest id are the list's.
#[test]
fn a_listed_coalition_that_answers_nothing_is_dropped() {
    let usage = |id| match id {
        307 => Some(mds_record()),
        9 => Some([0; USAGE_WORDS]),
        _ => None,
    };
    let read = read_of(&[(307, 1), (2, 0), (400, 1), (9, 1)], true, usage, (125, 3));
    assert_eq!(
        read,
        CoalitionRead {
            listed: 4,
            newest: Some(400),
            truncated: true,
            rows: vec![decode_usage(307, &mds_record(), (125, 3)).expect("decodes")],
        }
    );
}

#[test]
fn the_list_decodes_whole_resource_entries() {
    let entry = |id: u64, kind: u32, tasks: u32| {
        let mut e = id.to_ne_bytes().to_vec();
        e.extend(kind.to_ne_bytes());
        e.extend(tasks.to_ne_bytes());
        e
    };
    let mut bytes = [
        entry(1035, 0, 1),
        entry(557, 0, 1),
        entry(2, 0, 0),
        entry(9, 1, 3),
    ]
    .concat();
    bytes.extend(&entry(77, 0, 1)[..LIST_ENTRY - 1]);
    assert_eq!(decode_list(&bytes), vec![(1035, 1), (557, 1), (2, 0)]);
    assert_eq!(decode_list(&[]), Vec::new());
}

#[test]
fn the_list_room_is_the_size_answer_plus_headroom() {
    assert_eq!(list_room(9_216), Some(576 + LIST_HEADROOM));
    assert_eq!(list_room(0), None, "libproc's failure");
    assert_eq!(list_room(-1), None);
    assert_eq!(list_room(i64::MAX), Some(MAX_COALITIONS));
}

#[cfg(target_os = "macos")]
mod live {
    use super::super::imp;
    use super::*;

    #[test]
    fn the_self_check_passes_and_this_process_is_listed_with_its_counters() {
        let me = std::process::id();
        let id = self_check(me).expect("the lane reads right on this build");
        assert_eq!(crate::caps().coalitions, crate::Cap::Yes);
        assert_eq!(coalition_of(me), Some(id));
        let read = coalitions().expect("the list reads");
        assert!(read.listed > 0 && !read.truncated, "{read:?}");
        assert!(read.rows.len() <= read.listed as usize);
        assert!(read.newest.is_some_and(|n| n >= id), "{read:?}");
        let mine = read
            .rows
            .iter()
            .find(|r| r.id == id)
            .expect("this process's coalition answers");
        assert!(mine.tasks_started >= 1 && mine.cpu_ns > 0, "{mine:?}");
        assert!(mine.eqos_ns.iter().any(|ns| *ns > 0), "{mine:?}");
        assert_ne!(mine.footprint, Some(0), "this process is a live member");
        let root = coalition_of(1).expect("launchd's coalition");
        assert!(read.rows.iter().any(|r| r.id == root), "{root}");
    }

    /// The size answer for a null buffer is the BYTES the whole list takes
    /// (xnu's, which no SDK header documents): whole entries, and a buffer
    /// of that many plus the headroom holds the list without filling.
    #[test]
    fn the_null_buffer_answer_is_the_bytes_the_whole_list_takes() {
        let need = imp::list_need();
        let need_bytes = usize::try_from(need).expect("a size, not a failure");
        assert!(need_bytes > 0 && need_bytes % LIST_ENTRY == 0, "{need}");
        let room = list_room(i64::from(need)).expect("a room");
        let (list, wrote) = imp::list_in(room).expect("the list reads");
        assert!(wrote % LIST_ENTRY == 0, "{wrote}");
        assert!(
            wrote < room * LIST_ENTRY,
            "the size answer left room: {wrote}"
        );
        assert!(
            wrote.abs_diff(need_bytes) <= LIST_HEADROOM * LIST_ENTRY,
            "the list ({wrote} bytes) is the size answer's ({need_bytes} bytes)"
        );
        assert_eq!(
            list.len(),
            wrote / LIST_ENTRY,
            "every entry is a resource one"
        );
    }
}

/// A kept coalition id is read again only where its pid's version moved (an exec, or a new
/// process at a reused pid), where the last read missed, or with no version; a pid a sweep did
/// not ask about is dropped. NEGATIVE CONTROL: with the version unchanged, the kept id stands
/// though the stand-in kernel's answer moved, so each read again here is the version's doing.
#[test]
fn a_kept_coalition_is_read_again_only_when_its_pid_version_moved() {
    use std::cell::Cell;
    let kernel = Cell::new(Some(307));
    let reads = Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        kernel.get()
    };
    let mut ids = CoalitionIds::default();
    let mut sweep = |asks: &[(u32, Option<i32>)]| {
        ids.begin_sweep();
        let got: Vec<Option<u64>> = asks.iter().map(|&(pid, v)| ids.of(pid, v, read)).collect();
        ids.end_sweep();
        (got, ids.len())
    };
    assert_eq!(sweep(&[(387, Some(10))]), (vec![Some(307)], 1));
    kernel.set(Some(491));
    assert_eq!(
        sweep(&[(387, Some(10))]),
        (vec![Some(307)], 1),
        "kept: no exec"
    );
    assert_eq!(reads.take(), 1);

    assert_eq!(sweep(&[(387, Some(11))]), (vec![Some(491)], 1), "an exec");
    assert_eq!(sweep(&[(387, Some(11))]), (vec![Some(491)], 1));
    assert_eq!(reads.take(), 1, "read once after the exec, then kept");

    kernel.set(Some(500));
    assert_eq!(
        sweep(&[(387, Some(40))]),
        (vec![Some(500)], 1),
        "a new process at the pid"
    );
    assert_eq!(
        sweep(&[]),
        (vec![], 0),
        "a pid no sweep asked about is dropped"
    );
    assert_eq!(
        sweep(&[(387, Some(40))]),
        (vec![Some(500)], 1),
        "and read again once back"
    );
    assert_eq!(reads.take(), 2);

    kernel.set(None);
    assert_eq!(
        sweep(&[(9, Some(5))]),
        (vec![None], 0),
        "a miss is not kept"
    );
    kernel.set(Some(12));
    assert_eq!(
        sweep(&[(9, Some(5))]),
        (vec![Some(12)], 1),
        "and asked again"
    );
    assert_eq!(
        sweep(&[(9, None)]),
        (vec![Some(12)], 0),
        "no version: read, not kept"
    );
    assert_eq!(sweep(&[(9, None)]), (vec![Some(12)], 0));
    assert_eq!(reads.take(), 4);
}

/// A SWEEP KEEPS NO COALITION ITS PID'S VERSION MOVED ACROSS ([`CoalitionIds::read`]): where the
/// version read after the coalition is not the one read before it (an exec, or a new process at
/// the pid, between the reads), the read answers `None` and keeps nothing, so the next sweep reads
/// the coalition again. NEGATIVE CONTROL: with the version the same across the read, the id is
/// kept and the next sweep reads no coalition.
#[test]
fn a_coalition_read_its_pid_version_moved_across_is_not_kept() {
    use std::cell::Cell;
    let reads = Cell::new(0);
    let coalition = || {
        reads.set(reads.get() + 1);
        Some(307)
    };
    let mut ids = CoalitionIds::default();
    let mut sweep = |versions: &[i32]| {
        let mut versions = versions.iter().copied();
        ids.begin_sweep();
        let got = ids.read(387, || versions.next(), coalition);
        ids.end_sweep();
        (got, ids.kept(387), versions.next())
    };
    assert_eq!(
        sweep(&[10, 11]),
        ((None, Some(10)), None, None),
        "an exec between the reads: nothing kept"
    );
    assert_eq!(reads.take(), 1);
    assert_eq!(
        sweep(&[11, 11]),
        ((Some(307), Some(11)), Some((11, 307)), None),
        "read again, the version the same across it: kept"
    );
    assert_eq!(reads.take(), 1);
    assert_eq!(
        sweep(&[11, 11]),
        ((Some(307), Some(11)), Some((11, 307)), Some(11)),
        "kept: the version read once, the coalition not at all"
    );
    assert_eq!(reads.take(), 0);
}
