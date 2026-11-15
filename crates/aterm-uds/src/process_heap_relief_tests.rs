// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ReliefRule, heap_footprint, heap_relief_owed, release_free_memory, relieve_heap_if_due,
};
use std::time::Duration;

const CHILD_ENV: &str = "ATERM_UDS_TEST_HEAP_RELIEF_CHILD";
const NAME: &str =
    "process::heap_relief_tests::a_burst_of_threads_leaves_its_frees_resident_until_a_release";
const FLOOR: u64 = 32 << 20;
const HOUR: Duration = Duration::from_secs(3600);

/// A floor of `floor`, no spacing and an hour's sweep, unless a phase names its own.
const fn rule(floor: u64) -> ReliefRule {
    ReliefRule {
        floor,
        spacing: Duration::ZERO,
        sweep: HOUR,
    }
}

/// Eight short-lived threads each fill 400 buffers of 8 to 120 KiB (under glibc's 128 KiB mmap
/// threshold, so its arenas serve them) and keep a 4 KiB survivor after every twelfth: about
/// 200 MB, each run of buffers below a survivor. The threads free their buffers, or with `keep`
/// hand them back live, to be freed later behind the survivors all the same.
fn stranding_burst(keep: bool) -> (Vec<Vec<u8>>, Vec<Box<[u8]>>) {
    let threads = (0..8u64)
        .map(|seed| {
            std::thread::spawn(move || {
                let mut survivors = Vec::new();
                let buffers = (0..400u64)
                    .map(|i| {
                        let len = (8 << 10) + (seed * 7919 + i * 104_729) % (112 << 10);
                        let buffer = vec![1u8; usize::try_from(len).unwrap()];
                        if i % 12 == 0 {
                            survivors.push(vec![3u8; 4096].into_boxed_slice());
                        }
                        std::hint::black_box(buffer)
                    })
                    .collect::<Vec<_>>();
                if keep {
                    (buffers, survivors)
                } else {
                    drop(std::hint::black_box(buffers));
                    (Vec::new(), survivors)
                }
            })
        })
        .collect::<Vec<_>>();
    let mut held = (Vec::new(), Vec::new());
    for thread in threads {
        let (buffers, survivors) = thread.join().expect("a burst thread");
        held.0.extend(buffers);
        held.1.extend(survivors);
    }
    held
}

#[test]
fn a_burst_of_threads_leaves_its_frees_resident_until_a_release() {
    if std::env::var_os(CHILD_ENV).is_none() {
        // Alone in a fresh process, so its resident size is this test's.
        let out = crate::deadline::ends_within(
            crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD_ENV, "1"),
            crate::deadline::ECHO_HANG,
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("HEAP RELIEF ok"),
            "{:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let footprint = || heap_footprint().expect("this process's anonymous memory");
    release_free_memory();
    let base = footprint();
    // The first ask: the burst's growth is measured from here.
    let _ = heap_relief_owed(rule(FLOOR));
    let (_, survivors) = stranding_burst(false);
    let burst = footprint();
    // The control: glibc kept the burst's frees resident, so a release has them to return.
    assert!(
        burst >= base + 3 * FLOOR,
        "the burst's frees stayed resident: {base} -> {burst}"
    );
    assert_eq!(
        relieve_heap_if_due(rule(FLOOR)),
        None,
        "grown since the previous ask, {base} -> {burst} owes nothing yet"
    );
    let relief = relieve_heap_if_due(rule(FLOOR))
        .unwrap_or_else(|| panic!("settled at {burst}, a release is owed"));
    let after = footprint();
    assert!(
        relief.before >= base + 3 * FLOOR && relief.after < base + FLOOR && after < base + FLOOR,
        "the release returned the frees: {base} -> {burst} -> {relief:?}, {after}"
    );
    // What the release left is the mark: a MiB past it, settled, is not yet a MiB's growth.
    assert_eq!(
        heap_relief_owed(rule(1 << 20)),
        None,
        "the release reset what is owed at {after}"
    );
    // A second burst, released, then live growth with no ask between: the growth is measured from
    // what the release left, so it is still growing, not settled against the burst's peak.
    let (_, more) = stranding_burst(false);
    let _ = heap_relief_owed(rule(FLOOR));
    let second = relieve_heap_if_due(rule(FLOOR))
        .unwrap_or_else(|| panic!("the second burst settled, a release is owed"));
    assert!(
        second.before >= second.after + 3 * FLOOR,
        "the second burst stranded its frees too: {second:?}"
    );
    let live = std::hint::black_box(vec![5u8; 48 << 20]);
    let grown = footprint();
    assert_eq!(
        heap_relief_owed(rule(FLOOR)),
        None,
        "grown since the release, {second:?} -> {grown} owes nothing yet"
    );
    // Settled 48 MiB past the release: owed once the spacing since that release has passed.
    let spaced = |spacing| ReliefRule {
        spacing,
        ..rule(FLOOR)
    };
    assert_eq!(
        heap_relief_owed(spaced(HOUR)),
        None,
        "settled at {grown}, but the last release was just now"
    );
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        heap_relief_owed(spaced(Duration::from_millis(10))).is_some(),
        "settled at {grown}, {} MiB past the release and its spacing, a release is owed",
        (grown - second.after) >> 20
    );
    // A release made elsewhere while the growth is live (the window's rewind writer, mid-burst)
    // returns little and moves nothing the relief measures from: it is still owed.
    release_free_memory();
    assert!(
        heap_relief_owed(rule(FLOOR)).is_some(),
        "a release elsewhere at {grown} left the relief owed"
    );
    drop(live);
    // A burst still live when the relief releases (a round waiting on its peers) leaves the mark
    // at its plateau; its frees, stranded once it ends, show in no growth, and the sweep owes them.
    let (buffers, held) = stranding_burst(true);
    let _ = heap_relief_owed(rule(FLOOR));
    let plateau = relieve_heap_if_due(rule(FLOOR))
        .unwrap_or_else(|| panic!("settled at the live burst's plateau, a release is owed"));
    assert!(
        plateau.before < plateau.after + FLOOR,
        "the burst was live, so the release returned little: {plateau:?}"
    );
    drop(std::hint::black_box(buffers));
    let stranded = footprint();
    assert_eq!(
        heap_relief_owed(rule(FLOOR)),
        None,
        "no growth past the plateau's release at {stranded}, owed by none"
    );
    std::thread::sleep(Duration::from_millis(20));
    let swept = relieve_heap_if_due(ReliefRule {
        sweep: Duration::from_millis(10),
        ..rule(FLOOR)
    })
    .unwrap_or_else(|| panic!("settled at {stranded} past the sweep, a release is owed"));
    assert!(
        swept.after + 3 * FLOOR <= swept.before,
        "the sweep returned what the burst stranded after its release: {plateau:?} -> {swept:?}"
    );
    drop(held);
    drop(more);
    drop(survivors);
    println!(
        "HEAP RELIEF ok: {base} -> {burst} -> {after} -> {second:?} -> {grown} -> {plateau:?} -> \
         {swept:?}"
    );
}
