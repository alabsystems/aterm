// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TALLY COUNTS WHAT THE SEAM RECORDS. Every flush aterm-durable makes is one of
//! astream-sys's calls (`aterm_durable::ASTREAM_COUNTS` says `all`), so
//! `astream_sys::flush_tally` moves by exactly the flushes `aterm_durable::seam` records,
//! kind by kind. One test in a binary of its own, because the tally is process-wide: here
//! nothing else flushes while it reads.

use std::io::Write as _;

use aterm_durable::{Flush, seam};

/// What `astream_sys::flush_tally` moved by between two readings: `(full, barrier, plain)`.
fn moved(before: astream_sys::FlushTally, after: astream_sys::FlushTally) -> (u64, u64, u64) {
    (
        after.full - before.full,
        after.barrier - before.barrier,
        after.plain - before.plain,
    )
}

/// An in-process durable replace (the new copy flushed in full, data then all, renamed over
/// the old, the directory's entry flushed in full) moves `full` by exactly the three durable
/// flushes the seam records and nothing else; an ordered write moves `barrier` by the one
/// ordered flush the seam records on Apple targets, where the barrier is its own call, and
/// `full` elsewhere, where it is a full flush. NEGATIVE CONTROL: a flush that calls the OS
/// directly (`File::sync_all`, `File::sync_data`), as `primitive::full` and
/// `primitive::full_data` did before they were astream-sys's, leaves the tally unmoved. On
/// Apple, the plain-flush fallback on a descriptor that refuses both of `ordered`'s flushes
/// (`/dev/null`) is one ordered flush to the seam and one call of each kind to the tally: the
/// refused barrier, the refused full flush answering it and the plain `fsync(2)` answering both.
#[test]
fn the_tally_moves_by_exactly_the_flushes_the_seam_records() {
    assert_eq!(aterm_durable::ASTREAM_COUNTS, "all");
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-durable-tally-")
        .tempdir()
        .expect("a scratch directory");
    let target = dir.path().join("state");
    std::fs::write(&target, b"old").expect("the old copy");

    let recording = seam::Recording::start();
    let before = astream_sys::flush_tally();
    let tmp = dir.path().join("state.tmp");
    let mut file = std::fs::File::create(&tmp).expect("the new copy");
    file.write_all(b"new").expect("its bytes");
    aterm_durable::durable_data(&file).expect("its data, flushed in full");
    Flush::Durable
        .file(&file)
        .expect("all of it, flushed in full");
    drop(file);
    std::fs::rename(&tmp, &target).expect("the replace");
    aterm_durable::durable_dir(dir.path()).expect("the entry, flushed in full");
    let after = astream_sys::flush_tally();
    let asked = recording.take();
    assert_eq!(
        asked,
        [Flush::Durable; 3],
        "the replace's flushes, as asked"
    );
    assert_eq!(
        moved(before, after),
        (3, 0, 0),
        "each durable flush the seam recorded is one full flush counted: {before:?} -> {after:?}"
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&target)
        .expect("the replaced copy");
    file.write_all(b" and more").expect("a later write");
    let before = astream_sys::flush_tally();
    Flush::Ordered.file(&file).expect("the ordered flush");
    let after = astream_sys::flush_tally();
    assert_eq!(recording.take(), [Flush::Ordered]);
    let ordered = if cfg!(target_vendor = "apple") {
        (0, 1, 0)
    } else {
        (1, 0, 0)
    };
    assert_eq!(
        moved(before, after),
        ordered,
        "the ordered flush the seam recorded is one call counted: {before:?} -> {after:?}"
    );

    let before = astream_sys::flush_tally();
    #[expect(
        clippy::disallowed_methods,
        reason = "the negative control: a flush that calls the OS directly, not through aterm-durable"
    )]
    let direct = file.sync_all().and_then(|()| file.sync_data());
    direct.expect("the direct flushes");
    assert_eq!(
        astream_sys::flush_tally(),
        before,
        "a flush that calls the OS directly is no call astream-sys counts"
    );
    assert_eq!(recording.take(), [], "nor one the seam records");

    #[cfg(target_vendor = "apple")]
    {
        let null = std::fs::File::open("/dev/null").expect("/dev/null opens");
        let before = astream_sys::flush_tally();
        let refused = aterm_durable::ordered(&null);
        assert!(
            refused.as_ref().is_err_and(astream_sys::barrier_refused),
            "devfs no longer refuses both flushes on /dev/null ({refused:?}): this test needs \
             another descriptor that does"
        );
        assert_eq!(
            moved(before, astream_sys::flush_tally()),
            (1, 1, 0),
            "the refused barrier and the full flush that answered it"
        );
        assert_eq!(recording.take(), [Flush::Ordered]);
        let before = astream_sys::flush_tally();
        aterm_durable::ordered_or_fsync(&null).expect("the refusals answered by fsync(2)");
        assert_eq!(
            moved(before, astream_sys::flush_tally()),
            (1, 1, 1),
            "one call of each kind answered one ordered flush"
        );
        assert_eq!(recording.take(), [Flush::Ordered]);
    }
}
