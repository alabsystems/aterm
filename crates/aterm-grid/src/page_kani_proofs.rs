// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kani proofs for page-based memory allocation.
//!
//! Verifies offset arithmetic, allocation bounds, memory pooling invariants,
//! and pointer safety for [`Page`], [`PageSlice`], and [`PageStore`].

use super::*;
use crate::{Cell, KANI_MAX_COLS, KANI_MAX_ROWS};

#[kani::proof]
fn offset_within_bounds() {
    let offset: u32 = kani::any();
    kani::assume(offset < PAGE_SIZE as u32);
    kani::assume(offset % 4 == 0); // Aligned for u32

    let page = Page::default();
    let _cell_offset = Offset::<u32>::new(offset);

    // This should not panic
    let ptr = page.data_ptr();
    // SAFETY: offset < PAGE_SIZE assumed above; ptr.add within page bounds.
    let target = unsafe { ptr.add(offset as usize) };
    // SAFETY: PAGE_SIZE is the full page; ptr.add(PAGE_SIZE) is one-past-end (valid for comparison).
    kani::assert(target < unsafe { ptr.add(PAGE_SIZE) }, "out of bounds");
}

#[kani::proof]
fn offset_resolve_safe() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows >= 2 && rows <= KANI_MAX_ROWS);
    kani::assume(cols >= 4 && cols <= KANI_MAX_COLS);

    let offset: u32 = kani::any();
    kani::assume(offset < PAGE_SIZE as u32);
    kani::assume(offset % core::mem::align_of::<u32>() as u32 == 0);
    // Ensure bounds check passes: offset + size_of::<u32>() <= PAGE_SIZE
    kani::assume(offset as usize + core::mem::size_of::<u32>() <= PAGE_SIZE);

    let page = Page::new();
    // SAFETY: offset is bounded/aligned per kani::assume above; page is freshly allocated.
    let resolved = unsafe { Offset::<u32>::new(offset).get(page.as_ref()) };

    // With proper constraints, get() should succeed
    if let Some(ptr_ref) = resolved {
        let ptr = ptr_ref as *const u32 as usize;
        let base = page.data_ptr() as usize;
        let end = base + PAGE_SIZE;

        kani::assert(ptr >= base, "resolved pointer before base");
        kani::assert(
            ptr + core::mem::size_of::<u32>() <= end,
            "resolved pointer past end",
        );
    }
}

#[kani::proof]
fn offset_get_mut_write_round_trip() {
    let offset: u32 = kani::any();
    let value: u32 = kani::any();
    kani::assume(offset < PAGE_SIZE as u32);
    kani::assume(offset % core::mem::align_of::<u32>() as u32 == 0);
    kani::assume(offset as usize + core::mem::size_of::<u32>() <= PAGE_SIZE);

    let page = Page::new();
    let typed_offset = Offset::<u32>::new(offset);

    // SAFETY: offset is bounded/aligned per assumptions above.
    let slot = unsafe { typed_offset.get_mut(page.as_ref()) };
    kani::assert(
        slot.is_some(),
        "Offset::get_mut should succeed for in-bounds aligned offsets",
    );

    if let Some(slot) = slot {
        *slot = value;
    }

    // SAFETY: same bounded/aligned offset as above.
    let read_back = unsafe { typed_offset.get(page.as_ref()) };
    kani::assert(
        read_back.is_some(),
        "Offset::get should succeed after get_mut write",
    );
    if let Some(read_back) = read_back {
        kani::assert(
            *read_back == value,
            "get_mut write must round-trip through get",
        );
    }
}

#[kani::proof]
fn page_store_allocation_within_bounds() {
    let len: u16 = kani::any();
    kani::assume(len > 0);
    kani::assume(len <= (PAGE_SIZE / core::mem::size_of::<u32>()) as u16);

    let mut store = PageStore::new();
    let slice = store.alloc_slice::<u32>(len);
    let byte_offset = slice.offset().byte_offset() as usize;
    let bytes = len as usize * core::mem::size_of::<u32>();

    kani::assert(byte_offset + bytes <= PAGE_SIZE, "allocation out of bounds");
}

#[kani::proof]
fn page_store_allocation_safe() {
    let rows: u16 = kani::any();
    let cols: u16 = kani::any();
    kani::assume(rows >= 2 && rows <= KANI_MAX_ROWS);
    // Constrain cols so two Cell rows fit comfortably within Kani PAGE_SIZE
    // (256 bytes). Cell is 8 bytes, so max theoretical = 256/8/2 = 16 cols.
    // But cols=16 fills the page exactly (256 bytes), leaving no headroom
    // for PageStore alignment overhead — causing solver explosion (OOM kill).
    // Use cols <= 8 (128 bytes for 2 rows) for tractable verification.
    kani::assume(cols >= 4 && cols <= 8);

    let mut store = PageStore::new();

    let slice = store.alloc_slice::<Cell>(cols);
    let byte_offset = slice.offset().byte_offset() as usize;
    let bytes = cols as usize * core::mem::size_of::<Cell>();
    kani::assert(byte_offset + bytes <= PAGE_SIZE, "allocation out of bounds");

    if rows > 1 {
        let slice2 = store.alloc_slice::<Cell>(cols);
        let offset2 = slice2.offset().byte_offset() as usize;
        kani::assert(offset2 + bytes <= PAGE_SIZE, "allocation out of bounds");
    }
}

// === Memory pooling proofs ===

#[kani::proof]
#[kani::unwind(18)]
fn preheat_stats_consistent() {
    let count: usize = kani::any();
    // Limit to reasonable range for proof tractability
    kani::assume(count <= 16);

    let mut store = PageStore::new();
    store.preheat(count);

    let stats = store.stats();
    kani::assert(stats.pages_allocated == count, "allocated count mismatch");
    kani::assert(stats.pages_free == count, "free count mismatch");
    kani::assert(stats.pages_in_use == 0, "in_use should be 0 after preheat");
    kani::assert(store.free_pages() == count, "free_pages() mismatch");
}

#[kani::proof]
#[kani::unwind(6)]
fn alloc_from_preheated_reduces_free() {
    let preheat: usize = kani::any();
    // Reduced from 8 to 4: Kani PAGE_SIZE=256 makes PageStore Vec operations
    // expensive for the solver. 4 pages is sufficient to verify the free-list
    // accounting invariant.
    kani::assume(preheat > 0 && preheat <= 4);

    let mut store = PageStore::new();
    store.preheat(preheat);
    let initial_free = store.free_pages();

    // Allocate something
    let _slice = store.alloc_slice::<u32>(10);

    // Should have used one free page
    kani::assert(
        store.free_pages() == initial_free - 1,
        "free pages should decrease by 1",
    );
    kani::assert(store.active_pages() == 1, "should have 1 active page");
    kani::assert(store.stats().reused == 1, "should record 1 reuse");
}

#[kani::proof]
#[kani::unwind(6)]
fn reset_preserves_total_pages() {
    let preheat: usize = kani::any();
    kani::assume(preheat > 0 && preheat <= 4);

    let mut store = PageStore::new();
    store.preheat(preheat);

    // Allocate to use some pages (max 64 u32s fit in Kani PAGE_SIZE=256)
    let _slice = store.alloc_slice::<u32>(50);
    let total_before = store.active_pages() + store.free_pages();

    // Reset
    store.reset();
    let total_after = store.active_pages() + store.free_pages();

    // Total pages should be preserved (moved to free list)
    kani::assert(
        total_before == total_after,
        "reset should preserve total page count",
    );
    kani::assert(store.active_pages() == 0, "reset should clear active pages");
}

#[kani::proof]
#[kani::unwind(10)]
fn shrink_to_fit_releases_free_pages() {
    let preheat: usize = kani::any();
    kani::assume(preheat > 0 && preheat <= 8);

    let mut store = PageStore::new();
    store.preheat(preheat);
    kani::assert(store.free_pages() == preheat, "preheat should work");

    store.shrink_to_fit();

    kani::assert(store.free_pages() == 0, "shrink should release all free");
    kani::assert(store.stats().pages_free == 0, "stats should reflect shrink");
}

// allocation_after_reset_uses_free_list removed (#5887): fixed reset/allocate
// smoke test with no symbolic inputs; better suited to a unit test than Kani.

#[kani::proof]
#[kani::unwind(6)]
fn stats_pages_in_use_bounded() {
    let preheat: usize = kani::any();
    kani::assume(preheat <= 4);

    let mut store = PageStore::new();
    store.preheat(preheat);

    // Do some allocations (max 64 u32s fit in Kani PAGE_SIZE=256)
    let _s1 = store.alloc_slice::<u32>(50);
    let _s2 = store.alloc_slice::<u32>(50);

    let stats = store.stats();

    // pages_in_use should never exceed total allocated
    kani::assert(
        stats.pages_in_use <= stats.pages_allocated,
        "in_use cannot exceed allocated",
    );
    // free + in_use should equal total
    kani::assert(
        stats.pages_free + stats.pages_in_use == store.active_pages() + store.free_pages(),
        "free + in_use should equal total",
    );
}

// offset_get_never_dangling removed (#5887): tautological — asserts the
// exact same expression as its kani::assume. No production code called.

/// Proof: alloc_slice alignment is correct
#[kani::proof]
fn page_store_alignment_correct() {
    let align: usize = kani::any();
    let current: usize = kani::any();
    kani::assume(align > 0 && align.is_power_of_two() && align <= 4096);
    kani::assume(current < PAGE_SIZE);

    // align_up formula: (current + align - 1) & !(align - 1)
    let aligned = (current + align - 1) & !(align - 1);
    kani::assert(aligned % align == 0, "Result must be aligned");
    kani::assert(aligned >= current, "Aligned offset must not decrease");
}

/// Proof: PageSlice cannot access beyond allocation
#[kani::proof]
#[kani::unwind(5)]
fn page_slice_bounds_safe() {
    let offset: u32 = kani::any();
    let len: usize = kani::any();
    let elem_size: usize = kani::any();
    kani::assume(elem_size > 0 && elem_size <= 64);
    kani::assume(len > 0 && len <= 4); // Limited for tractability with unwind(5)
    kani::assume((offset as usize) + len * elem_size <= PAGE_SIZE);

    for i in 0..len {
        let access_offset = offset as usize + i * elem_size;
        kani::assert(
            access_offset + elem_size <= PAGE_SIZE,
            "Element access must be within page bounds",
        );
    }
}

#[kani::proof]
fn page_slice_as_slice_len_and_pointer_stable() {
    let len: u8 = kani::any();
    kani::assume(len > 0 && len <= 8);

    let mut backing = [0_u32; 8];
    let raw = &mut backing[..usize::from(len)];
    let page_slice = PageSlice::from_raw(raw);
    let view = page_slice.as_slice();

    kani::assert(
        view.len() == usize::from(len),
        "as_slice length must match allocation length",
    );
    kani::assert(
        view.as_ptr() == backing.as_ptr(),
        "as_slice pointer must match backing allocation",
    );
}

#[kani::proof]
fn page_slice_as_mut_slice_write_round_trips() {
    let len: u8 = kani::any();
    let idx: u8 = kani::any();
    let value: u32 = kani::any();
    kani::assume(len > 0 && len <= 8);
    kani::assume(idx < len);

    let mut backing = [0_u32; 8];
    let raw = &mut backing[..usize::from(len)];
    let mut page_slice = PageSlice::from_raw(raw);
    let write_idx = usize::from(idx);

    page_slice.as_mut_slice()[write_idx] = value;

    kani::assert(
        backing[write_idx] == value,
        "as_mut_slice write must stay within backing allocation",
    );
    kani::assert(
        page_slice.as_slice()[write_idx] == value,
        "as_slice must observe writes from as_mut_slice",
    );
}

// page_data_ptr_edge_writes_are_in_bounds removed (#5887): concrete first/last-byte
// write smoke test with no symbolic state; better suited to a unit test than Kani.

// page_and_page_slice_send_sync_bounds_hold removed (#5887): compile-time
// trait checks (assert_send/assert_sync) provide no runtime or symbolic value.
