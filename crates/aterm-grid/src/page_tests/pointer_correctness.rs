// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

// ========================================================================
// Alignment and pointer correctness tests (MIRI-exercised)
// ========================================================================

#[test]
fn alloc_slice_alignment_u8() {
    let mut store = PageStore::new();
    let slice = store.alloc_slice::<u8>(100);
    assert_eq!(slice.len(), 100);
    // u8 has align 1, any address works
    let ptr = slice.as_slice().as_ptr();
    assert!(!ptr.is_null());
}

#[test]
fn alloc_slice_alignment_u64() {
    let mut store = PageStore::new();
    // First alloc a u8 to misalign, then alloc u64 which needs align 8
    let _byte = store.alloc_slice::<u8>(1);
    let slice = store.alloc_slice::<u64>(10);
    assert_eq!(slice.len(), 10);
    // Verify alignment
    let ptr = slice.as_slice().as_ptr();
    assert_eq!(ptr.align_offset(std::mem::align_of::<u64>()), 0);
}

#[test]
fn alloc_slice_alignment_u128() {
    let mut store = PageStore::new();
    // u128 needs 16-byte alignment on most platforms
    let _byte = store.alloc_slice::<u8>(3); // misalign
    let slice = store.alloc_slice::<u128>(5);
    assert_eq!(slice.len(), 5);
    let ptr = slice.as_slice().as_ptr();
    assert_eq!(ptr.align_offset(std::mem::align_of::<u128>()), 0);
}

#[test]
fn alloc_slice_sequential_no_overlap() {
    let mut store = PageStore::new();
    let slice1 = store.alloc_slice::<u32>(100);
    let slice2 = store.alloc_slice::<u32>(100);

    // Verify the two slices don't overlap
    let ptr1 = slice1.as_slice().as_ptr() as usize;
    let end1 = ptr1 + 100 * std::mem::size_of::<u32>();
    let ptr2 = slice2.as_slice().as_ptr() as usize;

    assert!(
        end1 <= ptr2,
        "slices overlap: first ends at {end1:#x}, second starts at {ptr2:#x}"
    );
}

#[test]
fn alloc_slice_write_read_roundtrip_after_reset() {
    let mut store = PageStore::new();

    // Allocate, write, reset, re-allocate, verify clean
    let mut slice = store.alloc_slice::<u64>(50);
    for i in 0..50 {
        slice[i] = 0xCAFE_BABE_0000_0000 + i as u64;
    }
    // Verify write
    for i in 0..50 {
        assert_eq!(slice[i], 0xCAFE_BABE_0000_0000 + i as u64);
    }

    store.reset();
    let new_slice = store.alloc_slice::<u64>(50);
    // After reset + lazy zeroing, all values should be zero
    for &val in new_slice.iter() {
        assert_eq!(val, 0);
    }
}

#[test]
fn offset_out_of_bounds_returns_none() {
    let page = Page::new();
    // Offset at PAGE_SIZE boundary should return None
    let offset = Offset::<u32>::new(PAGE_SIZE as u32);
    let result = unsafe { offset.get(&page) };
    assert!(result.is_none());
}

#[test]
fn offset_near_boundary_u32() {
    let page = Page::new();
    // Last valid u32 offset: PAGE_SIZE - 4
    let last_valid = (PAGE_SIZE - std::mem::size_of::<u32>()) as u32;
    let offset = Offset::<u32>::new(last_valid);
    let result = unsafe { offset.get(&page) }
        .expect("last in-bounds u32 offset should return a readable value");
    assert_eq!(*result, 0); // page is zeroed

    // One byte past: should fail
    let one_past = last_valid + 1;
    let offset_past = Offset::<u32>::new(one_past);
    let result_past = unsafe { offset_past.get(&page) };
    assert!(result_past.is_none());
}

#[test]
fn reset_three_pages_all_zeroed_on_reuse() {
    let mut store = PageStore::new();
    // Page 0: 100 u32s = 400 bytes. next_offset = 400.
    let mut s1 = store.alloc_slice::<u32>(100);
    for i in 0..100 {
        s1[i] = 0x11111111;
    }
    // Force page 1: 16384 u32s = 65536 bytes > remaining (65136).
    let mut s2 = store.alloc_slice::<u32>(16384);
    for i in 0..16384 {
        s2[i] = 0x22222222;
    }
    // Force page 2: 16384 u32s again exceeds page 1's remaining space (0).
    let mut s3 = store.alloc_slice::<u32>(1000);
    for i in 0..1000 {
        s3[i] = 0x33333333;
    }
    assert_eq!(store.active_pages(), 3, "expected exactly 3 active pages");
    store.reset();
    // Reallocate from recycled pages and verify zeroing.
    let c1 = store.alloc_slice::<u32>(100);
    for (i, &v) in c1.iter().enumerate() {
        assert_eq!(v, 0, "page1 idx {i}: {v:#010x}");
    }
    let c2 = store.alloc_slice::<u32>(16384);
    for (i, &v) in c2.iter().enumerate() {
        assert_eq!(v, 0, "page2 idx {i}: {v:#010x}");
    }
    let c3 = store.alloc_slice::<u32>(1000);
    for (i, &v) in c3.iter().enumerate() {
        assert_eq!(v, 0, "page3 idx {i}: {v:#010x}");
    }
}

// ========================================================================
// Memory safety tests: pointer stability and alignment
// ========================================================================
//
// These tests target the unsafe invariants that the PageStore must uphold:
// - Pointers from alloc_slice remain valid after subsequent allocations
// - Cell (the production type) gets correct alignment from alloc_slice

/// Verify that PageSlice<Cell> pointers are valid and properly aligned.
///
/// Cell is `repr(C, packed)` (align 1), but this test exercises the actual
/// production allocation path rather than proxy types (u8, u64).
#[test]
fn alloc_slice_cell_alignment_and_access() {
    use crate::Cell;

    let mut store = PageStore::new();

    // Allocate cells exactly as Row::new does
    let mut cells = store.alloc_slice::<Cell>(80);
    assert_eq!(cells.len(), 80);

    // Verify pointer is non-null and usable
    let ptr = cells.as_slice().as_ptr();
    assert!(!ptr.is_null());

    // Write and read back through the slice (exercises from_raw_parts)
    for cell in cells.iter_mut() {
        *cell = Cell::EMPTY;
    }
    for cell in cells.as_slice() {
        assert!(cell.is_empty());
    }
}

/// Verify zero-length allocations are stable and do not consume page capacity.
///
/// This exercises the `from_raw_parts` / `from_raw_parts_mut` boundary where
/// `len == 0` and ensures later allocations are unaffected.
#[test]
fn alloc_zero_length_slice_is_stable_and_non_consuming() {
    let mut store = PageStore::new();

    let mut empty = store.alloc_slice::<u32>(0);
    assert_eq!(empty.len(), 0);
    assert!(empty.as_slice().is_empty());
    assert!(empty.as_mut_slice().is_empty());
    let empty_ptr = empty.as_slice().as_ptr();
    let empty_mut_ptr = empty.as_mut_slice().as_mut_ptr();
    assert_eq!(empty_ptr, empty_mut_ptr);
    let empty_offset = empty.offset().byte_offset();
    let empty_page_id = empty.page_id();

    let mut filled = store.alloc_slice::<u32>(4);
    assert_eq!(filled.page_id(), empty_page_id);
    assert_eq!(
        filled.offset().byte_offset(),
        empty_offset,
        "len=0 allocation must not advance page offset"
    );
    for (i, value) in filled.iter_mut().enumerate() {
        *value = i as u32 + 1;
    }
    assert_eq!(filled.as_slice(), &[1, 2, 3, 4]);
}

/// Verify that earlier PageSlice pointers remain valid after subsequent
/// allocations on the same page.
///
/// This is the core safety property of the arena allocator: alloc_slice
/// returns a NonNull<T> derived from a Box<Page> that stays pinned in
/// the pages Vec. If the Vec reallocated or pages moved, these pointers
/// would dangle.
#[test]
fn alloc_slice_pointer_stability_across_allocations() {
    let mut store = PageStore::new();

    // Allocate three slices on the same page
    let mut s1 = store.alloc_slice::<u32>(100);
    let mut s2 = store.alloc_slice::<u32>(100);
    let mut s3 = store.alloc_slice::<u32>(100);

    // Write distinct patterns to each
    for (i, val) in s1.iter_mut().enumerate() {
        *val = 0xAAAA_0000 + i as u32;
    }
    for (i, val) in s2.iter_mut().enumerate() {
        *val = 0xBBBB_0000 + i as u32;
    }
    for (i, val) in s3.iter_mut().enumerate() {
        *val = 0xCCCC_0000 + i as u32;
    }

    // Force more allocations that may grow the pages Vec
    for _ in 0..10 {
        let _extra = store.alloc_slice::<u32>(1000);
    }

    // Verify original slices still have correct data (pointers didn't dangle)
    for (i, &val) in s1.iter().enumerate() {
        assert_eq!(
            val,
            0xAAAA_0000 + i as u32,
            "s1[{i}] corrupted after subsequent allocations"
        );
    }
    for (i, &val) in s2.iter().enumerate() {
        assert_eq!(
            val,
            0xBBBB_0000 + i as u32,
            "s2[{i}] corrupted after subsequent allocations"
        );
    }
    for (i, &val) in s3.iter().enumerate() {
        assert_eq!(
            val,
            0xCCCC_0000 + i as u32,
            "s3[{i}] corrupted after subsequent allocations"
        );
    }
}

/// Verify that PageSlice pointers remain valid when allocations span
/// multiple pages.
///
/// When the first page fills up, alloc_slice creates a new page. The old
/// page must not be moved or freed; the NonNull<T> from the first
/// allocation must still point to valid memory.
#[test]
fn alloc_slice_pointer_stability_across_pages() {
    use crate::Cell;

    let mut store = PageStore::new();

    // Allocate a Cell slice that fills most of the first page
    // Cell is 8 bytes, 8000 cells = 64000 bytes (nearly fills 64KB page)
    let mut first_page_cells = store.alloc_slice::<Cell>(8000);
    for cell in first_page_cells.iter_mut() {
        cell.set_char('X');
    }

    // This should spill to a new page
    let mut second_page_cells = store.alloc_slice::<Cell>(8000);
    for cell in second_page_cells.iter_mut() {
        cell.set_char('Y');
    }

    assert_eq!(store.active_pages(), 2, "expected exactly 2 active pages");

    // Verify first page data is intact
    for (i, cell) in first_page_cells.as_slice().iter().enumerate() {
        assert_eq!(
            cell.char(),
            'X',
            "first page cell[{i}] corrupted after second page allocation"
        );
    }
    // Verify second page data is intact
    for (i, cell) in second_page_cells.as_slice().iter().enumerate() {
        assert_eq!(cell.char(), 'Y', "second page cell[{i}] corrupted");
    }
}
