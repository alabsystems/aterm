// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::alloc::{GlobalAlloc, Layout};

use super::{
    BINS, CLASSES, CompactZoneAlloc, MAX_ALIGNED_SLAB, class_index, os_unfair_lock_lock,
    os_unfair_lock_unlock, release_free_memory,
};

fn retained_cohort_slabs(idx: usize, cohort: &[usize]) -> usize {
    let cell = BINS[idx].0.get();
    // SAFETY: Test acquires the bin lock before walking `partial_head`.
    unsafe {
        os_unfair_lock_lock(&raw mut (*cell).lock);
        let mut n = 0usize;
        let mut cur = (*cell).partial_head;
        while !cur.is_null() {
            let base = super::slab_base_from_header(cur) as usize;
            n += usize::from(cohort.binary_search(&base).is_ok());
            cur = (*cur).next;
        }
        os_unfair_lock_unlock(&raw mut (*cell).lock);
        n
    }
}

#[test]
fn size_class_lookup_satisfies_every_size_and_alignment() {
    for align in [
        1usize, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 4096, 16384,
    ] {
        for size in 1..=16_500 {
            if let Some(idx) = class_index(size, align) {
                let slot = CLASSES[idx];
                assert!(
                    slot >= size,
                    "slot {slot} < size {size} (align {align}, idx {idx})"
                );
                if align <= 16 {
                    assert_eq!(slot % 16, 0);
                    if idx > 0 {
                        assert!(CLASSES[idx - 1] < size);
                    }
                } else {
                    assert!(slot.is_power_of_two());
                    assert!(slot >= align);
                    assert!(MAX_ALIGNED_SLAB >= slot);
                }
            }
        }
    }
}

#[test]
fn compact_zone_handles_standard_overaligned_and_large_shrink_layouts() {
    for align in [1usize, 8, 16, 32, 64, 256, 4096, 16384, 65536] {
        for size in [
            1usize, 7, 16, 33, 48, 65, 257, 1024, 4096, 8192, 32768, 131072,
        ] {
            let layout = Layout::from_size_align(size, align).expect("valid layout");
            // SAFETY: `layout` has non-zero size (`size >= 1`), and each pointer is grown,
            // shrunk, and freed with its matching current layout.
            unsafe {
                let p = CompactZoneAlloc.alloc_zeroed(layout);
                assert!(!p.is_null());
                assert_eq!((p as usize) & (align - 1), 0);
                assert!(std::slice::from_raw_parts(p, size).iter().all(|&b| b == 0));
                std::ptr::write_bytes(p, 0x5A, size);

                let grown_size = size * 2 + 13;
                let q = CompactZoneAlloc.realloc(p, layout, grown_size);
                assert!(!q.is_null());
                assert_eq!((q as usize) & (align - 1), 0);
                assert!(
                    std::slice::from_raw_parts(q, size)
                        .iter()
                        .all(|&b| b == 0x5A)
                );

                let grown_layout =
                    Layout::from_size_align(grown_size, align).expect("valid grown layout");
                let mid_size = (size / 2).max(1);
                let mid = CompactZoneAlloc.realloc(q, grown_layout, mid_size);
                assert!(!mid.is_null());
                assert_eq!((mid as usize) & (align - 1), 0);
                assert!(
                    std::slice::from_raw_parts(mid, mid_size)
                        .iter()
                        .all(|&b| b == 0x5A)
                );

                let mid_layout =
                    Layout::from_size_align(mid_size, align).expect("valid mid layout");
                let shrunk = CompactZoneAlloc.realloc(mid, mid_layout, 1);
                assert!(!shrunk.is_null());
                assert_eq!((shrunk as usize) & (align - 1), 0);
                assert_eq!(*shrunk, 0x5A);

                let final_layout = Layout::from_size_align(1, align).expect("valid shrunk layout");
                CompactZoneAlloc.dealloc(shrunk, final_layout);
            }
        }
    }
}

#[test]
fn spike_and_drain_across_threads_flushes_magazines_and_unmaps_slabs() {
    const PROBE: &str = "ATERM_COMPACT_ZONE_SPIKE_PROBE";
    const CASE: &str =
        "compact_zone::tests::spike_and_drain_across_threads_flushes_magazines_and_unmaps_slabs";
    if std::env::var_os(PROBE).is_none() {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
            .env(PROBE, "1");
        let output =
            aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
                .expect("the isolated allocator workload finishes");
        assert!(output.status.success(), "allocator probe: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("compact-zone-spike-drained"));
        return;
    }
    // Other cases use this binary's global allocator. Only a private process can prove
    // that these slabs empty, with no unrelated allocation taking one of their slots.
    let layout = Layout::from_size_align(3_072, 16).expect("valid 3072 layout");
    let idx = class_index(layout.size(), layout.align()).expect("3072 uses slab bin");
    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let producer = std::thread::spawn(move || {
        // SAFETY: Pointers are allocated with `layout` and freed with the same `layout` on
        // the consumer thread.
        unsafe {
            for i in 0..96usize {
                let p = CompactZoneAlloc.alloc(layout);
                assert!(!p.is_null());
                std::ptr::write_bytes(p, (i as u8) | 1, layout.size());
                tx.send(p as usize).expect("send ptr");
            }
        }
    });
    let consumer = std::thread::spawn(move || {
        let mut cohort = Vec::with_capacity(96);
        // SAFETY: Every pointer came from `CompactZoneAlloc.alloc(layout)`.
        unsafe {
            while let Ok(addr) = rx.recv() {
                cohort.push(addr & !(super::SLAB_BYTES - 1));
                CompactZoneAlloc.dealloc(addr as *mut u8, layout);
            }
        }
        assert_eq!(cohort.len(), 96, "every spike allocation crossed threads");
        cohort.sort_unstable();
        cohort.dedup();
        assert!(cohort.len() > 2, "the spike spans multiple slabs");
        cohort
    });
    producer.join().expect("producer join");
    let cohort = consumer.join().expect("consumer join");
    release_free_memory();
    assert_eq!(
        retained_cohort_slabs(idx, &cohort),
        0,
        "thread-exit magazine flush + release_free_memory must unmap spike slabs"
    );
    // SAFETY: Allocate and free one zeroed slot with `layout`.
    unsafe {
        let z = CompactZoneAlloc.alloc_zeroed(layout);
        assert!(!z.is_null());
        assert!(
            std::slice::from_raw_parts(z, layout.size())
                .iter()
                .all(|&b| b == 0),
            "reused slab slots must be zeroed on alloc_zeroed"
        );
        CompactZoneAlloc.dealloc(z, layout);
    }
    println!("compact-zone-spike-drained");
}

/// One bin driven through its own state machine as the global path drives it (a slot from a
/// partial slab, else a fresh slab), on a bin and a budget no other thread touches: the
/// global bins serve this test binary's every thread.
struct Bench {
    state: super::BinState,
    idx: usize,
    retained: super::Retained,
    maps: usize,
    unmaps: usize,
}

impl Bench {
    fn new(size: usize, cap: usize) -> Self {
        Self {
            state: super::BinState {
                lock: super::OsUnfairLock(0),
                partial_head: std::ptr::null_mut(),
            },
            idx: class_index(size, 16).expect("a slab class"),
            retained: super::Retained::new(cap),
            maps: 0,
            unmaps: 0,
        }
    }

    fn take(&mut self) -> *mut u8 {
        // SAFETY: `self.state` is this bench's own bin, which no other thread reaches, and a
        // fresh slab joins it as the global path's does.
        unsafe {
            let p = super::pop_one_from_partial(&mut self.state, CLASSES[self.idx], &self.retained);
            if !p.is_null() {
                return p;
            }
            let slab = super::map_slab(self.idx, 1);
            assert!(!slab.is_null(), "a slab maps");
            self.maps += 1;
            super::push_partial_front(&mut self.state, super::header_from_slab_base(slab));
            slab
        }
    }

    fn give(&mut self, ptr: *mut u8) {
        // SAFETY: `ptr` is a live slot `take` handed out of this bench's bin.
        unsafe {
            let unmap = super::return_one_to_state(&mut self.state, ptr, &self.retained);
            if !unmap.is_null() {
                let _ = libc::munmap(unmap.cast(), super::SLAB_BYTES);
                self.unmaps += 1;
            }
        }
    }

    /// One round that takes `live` slots and gives every one back: the slabs it mapped and
    /// unmapped.
    fn round(&mut self, live: usize) -> (usize, usize) {
        let (maps, unmaps) = (self.maps, self.unmaps);
        let slots: Vec<*mut u8> = (0..live).map(|_| self.take()).collect();
        for slot in slots {
            self.give(slot);
        }
        (self.maps - maps, self.unmaps - unmaps)
    }

    /// What a relief does to the bin: every emptied slab unmapped. How many.
    fn relieve(&mut self) -> usize {
        let mut relieved = 0;
        loop {
            let mut out = [std::ptr::null_mut::<u8>(); 32];
            // SAFETY: the bench owns its bin; each slab taken is unmapped once.
            let taken =
                unsafe { super::take_empty_slabs(&mut self.state, &self.retained, &mut out) };
            for &slab in &out[..taken] {
                // SAFETY: `slab` was unlinked from the bin and nothing holds a slot in it.
                let _ = unsafe { libc::munmap(slab.cast(), super::SLAB_BYTES) };
            }
            relieved += taken;
            if taken < out.len() {
                return relieved;
            }
        }
    }

    fn slabs(&self) -> usize {
        let mut n = 0;
        let mut cur = self.state.partial_head;
        while !cur.is_null() {
            n += 1;
            // SAFETY: every slab in the bench's partial list is mapped and linked.
            cur = unsafe { (*cur).next };
        }
        n
    }
}

/// A reader that takes and gives back the same working set every round (1,200 bitsets of a
/// membership evaluation, 152 bytes each) maps its slabs in the first round and none after,
/// and unmaps none until a relief, which gives every one back.
///
/// NEGATIVE CONTROL (2026-11-06): with an emptied slab kept only when it is its class's only
/// partial slab (`Retained::keep` returning `lone`, the shape before), the first round already
/// unmaps all but one of the 15 slabs it mapped (`left: (15, 14), right: (15, 0)`), and every
/// later round maps them again.
#[test]
fn a_round_that_gives_back_what_it_took_reuses_its_slabs() {
    let mut bench = Bench::new(152, super::RETAINED_EMPTY_BYTES);
    let per_slab = usize::from(super::SLAB_CAPS[bench.idx]);
    let slabs = 1_200usize.div_ceil(per_slab);
    assert_eq!(bench.round(1_200), (slabs, 0), "the first round");
    for _ in 1..20 {
        assert_eq!(bench.round(1_200), (0, 0), "a later round");
    }
    assert_eq!(bench.slabs(), slabs);
    assert_eq!(
        bench
            .retained
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed),
        slabs * super::SLAB_BYTES
    );
    assert_eq!(bench.relieve(), slabs, "a relief gives every slab back");
    assert_eq!(bench.slabs(), 0);
    assert_eq!(
        bench
            .retained
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

/// The slabs kept stay within the cap in bytes: a working set of 20 slabs under a cap of 4
/// keeps 4 and unmaps the rest, maps 16 again each round, and a relief gives back the 4.
///
/// NEGATIVE CONTROL (2026-11-06): with `Retained::keep` keeping every emptied slab (no cap),
/// the first round unmaps none of the 20 (`left: (20, 0), right: (20, 16)`).
#[test]
fn the_slabs_kept_stay_within_the_cap() {
    const CAP_SLABS: usize = 4;
    let mut bench = Bench::new(152, CAP_SLABS * super::SLAB_BYTES);
    let per_slab = usize::from(super::SLAB_CAPS[bench.idx]);
    let live = 20 * per_slab;
    assert_eq!(bench.round(live), (20, 20 - CAP_SLABS));
    assert_eq!(bench.slabs(), CAP_SLABS);
    assert_eq!(bench.round(live), (20 - CAP_SLABS, 20 - CAP_SLABS));
    assert_eq!(
        bench
            .retained
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed),
        CAP_SLABS * super::SLAB_BYTES
    );
    assert_eq!(bench.relieve(), CAP_SLABS);
}

/// The global zone's large path: blocks of one bucket come back from the cache zeroed on
/// `alloc_zeroed`. Which block comes back, and whether the bucket is empty after a relief,
/// are not asked here: `CompactZoneAlloc` is this test binary's global allocator, so every
/// other test thread frees into and pops from `LARGE_CACHE`, and one drains it
/// (`release_free_memory`), concurrently. The cache's own order is
/// [`large_cache_pops_the_hottest_block_and_drains_whole`]'s, on a cache of its own.
#[test]
fn large_blocks_come_back_zeroed_from_the_global_cache() {
    let layout = Layout::from_size_align(262_160, 16).expect("valid 256 KiB Arc<[u8]> layout");
    let mut ptrs = Vec::with_capacity(28);
    // SAFETY: Every pointer is allocated with `layout`, written within bounds, and freed with
    // the exact same `layout`.
    unsafe {
        for i in 0..28usize {
            let p = CompactZoneAlloc.alloc(layout);
            assert!(!p.is_null());
            std::ptr::write_bytes(p, (i as u8) | 1, layout.size());
            ptrs.push(p);
        }
        for p in ptrs.drain(..) {
            CompactZoneAlloc.dealloc(p, layout);
        }

        let reused = CompactZoneAlloc.alloc_zeroed(layout);
        assert!(!reused.is_null());
        assert!(
            std::slice::from_raw_parts(reused, layout.size())
                .iter()
                .all(|&b| b == 0),
            "reused large block must be zeroed on alloc_zeroed"
        );
        CompactZoneAlloc.dealloc(reused, layout);
        release_free_memory();
    }
}

/// THE LARGE CACHE'S ORDER, on a cache no other thread touches: a bucket pops the block
/// freed last (O(1) LIFO, the hottest pages), and draining oldest-first, as a relief
/// (`release_free_memory`) does, empties the bucket and the totals.
#[test]
fn large_cache_pops_the_hottest_block_and_drains_whole() {
    let mut cache = Box::new(super::LargeCacheState {
        lock: super::OsUnfairLock(0),
        initialized: false,
        free_head: super::NONE,
        lru_head: super::NONE,
        lru_tail: super::NONE,
        count: 0,
        total_bytes: 0,
        buckets: [super::NONE; super::LARGE_CACHE_BUCKETS],
        entries: [super::LargeEntry::EMPTY; super::LARGE_CACHE_SLOTS],
    });
    cache.ensure_init();
    let ps = super::page_size();
    let len = super::round_up_pow2(262_160, ps).expect("valid rounded len");
    let bucket = (len / ps) - 1;
    // The cache never reads through a block's pointer, so addresses alone stand in for them.
    let blocks: Vec<*mut u8> = (1..=28usize)
        .map(|i| std::ptr::without_provenance_mut(i * len))
        .collect();
    for &block in &blocks {
        cache.push_entry(bucket, block, len);
    }
    assert_eq!((cache.count, cache.total_bytes), (28, 28 * len));

    let head = cache.buckets[bucket];
    assert_ne!(head, super::NONE);
    assert_eq!(
        cache.unlink_entry(head),
        (blocks[27], len),
        "O(1) bucket LIFO pop must reuse the hottest 272 KiB block"
    );

    let mut drained = Vec::new();
    while cache.lru_head != super::NONE {
        let oldest = cache.lru_head;
        drained.push(cache.unlink_entry(oldest).0);
    }
    assert_eq!(drained, blocks[..27], "a relief drains oldest first");
    assert_eq!(cache.buckets[bucket], super::NONE);
    assert_eq!((cache.count, cache.total_bytes), (0, 0));
}
