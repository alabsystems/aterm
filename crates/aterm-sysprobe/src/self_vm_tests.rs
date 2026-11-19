// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The revision gate: rev 3's count is the least taken, and a count one short of it is refused,
/// as is a rev 2 record (which lacks every ledger this reader reads) and a count past the buffer.
/// The negative control is `REV3_WORDS - 1`: a gate that took any count would read the ledgers
/// of a rev 2 kernel as zeros, and that row fails it.
#[test]
fn self_vm_refuses_records_below_rev3() {
    assert_eq!(accept(REV3_WORDS), Ok(()));
    assert_eq!(accept(FULL_WORDS), Ok(()));
    assert_eq!(
        accept(REV3_WORDS - 1),
        Err(ReadError::Revision {
            words: REV3_WORDS - 1
        })
    );
    assert_eq!(accept(42), Err(ReadError::Revision { words: 42 }));
    assert_eq!(accept(0), Err(ReadError::Revision { words: 0 }));
    assert_eq!(
        accept(FULL_WORDS + 1),
        Err(ReadError::Revision {
            words: FULL_WORDS + 1
        })
    );
}

/// A rev 3 record's fields come through by name; the same record answered at a rev 2 count is
/// refused rather than read (the control for the field mapping is the distinct value in each).
/// The volatile purgeable words are taken only from the flavor that fills them: the negative
/// control is the same record read as `TASK_VM_INFO`'s, which carries none of its 14, 15, 16.
#[test]
fn self_vm_reads_each_field_from_its_own_slot() {
    let info = TaskVmInfo {
        phys_footprint: 11,
        internal: 12,
        compressed: 13,
        purgeable_volatile_pmap: 14,
        purgeable_volatile_resident: 15,
        purgeable_volatile_virtual: 16,
        ledger_purgeable_nonvolatile: 17,
        ledger_purgeable_volatile: 18,
        ledger_tag_graphics_footprint: 19,
        ledger_tag_graphics_footprint_compressed: 20,
        ledger_tag_graphics_nofootprint: 21,
        ledger_tag_graphics_nofootprint_compressed: 22,
        ledger_tag_media_footprint: 99,
        ..TaskVmInfo::default()
    };
    let plain = VmReading {
        words: REV3_WORDS,
        phys_footprint: 11,
        internal: 12,
        compressed: 13,
        purgeable: None,
        ledger_purgeable_nonvolatile: 17,
        ledger_purgeable_volatile: 18,
        ledger_tag_graphics_footprint: 19,
        ledger_tag_graphics_footprint_compressed: 20,
        ledger_tag_graphics_nofootprint: 21,
        ledger_tag_graphics_nofootprint_compressed: 22,
    };
    assert_eq!(reading(&info, REV3_WORDS, false), Ok(plain));
    assert_eq!(
        reading(&info, REV3_WORDS, true),
        Ok(VmReading {
            purgeable: Some(PurgeableVolatile {
                pmap: 14,
                resident: 15,
                virtual_size: 16,
            }),
            ..plain
        })
    );
    assert_eq!(
        reading(&info, 42, true),
        Err(ReadError::Revision { words: 42 }),
        "a rev 2 count is refused, never read"
    );
}

/// An inert live read of this process: the kernel answers at least rev 3 and a nonzero
/// footprint. It starts nothing and touches no GPU.
#[cfg(target_os = "macos")]
#[test]
fn self_vm_live_read_answers_rev3_or_later() {
    let vm = read().expect("TASK_VM_INFO on this task");
    assert!(vm.words >= REV3_WORDS && vm.words <= FULL_WORDS);
    assert!(vm.phys_footprint > 0);
    assert_eq!(
        vm.purgeable, None,
        "TASK_VM_INFO carries no purgeable words"
    );
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn vm_allocate(task: u32, address: *mut usize, size: usize, flags: i32) -> i32;
    fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
    fn vm_purgable_control(task: u32, address: usize, control: i32, state: *mut i32) -> i32;
}

/// 16 MiB of this task's own memory, allocated purgeable, touched and then marked volatile, is
/// counted resident by [`read_purgeable`]. The negative control is the `TASK_VM_INFO` record at
/// the same moment: its `purgeable_volatile_resident` slot stays zero with those 16 MiB
/// resident, which is why [`read`]'s reading carries no purgeable words. The region is this
/// test's own and is released before it returns; nothing else is started.
#[cfg(target_os = "macos")]
#[test]
fn self_vm_purgeable_read_counts_volatile_memory() {
    const BYTES: usize = 16 << 20;
    /// `VM_FLAGS_ANYWHERE | VM_FLAGS_PURGABLE` (`mach/vm_statistics.h`).
    const ANYWHERE_PURGABLE: i32 = 0x1 | 0x2;
    /// `VM_PURGABLE_SET_STATE` and `VM_PURGABLE_VOLATILE` (`mach/vm_purgable.h`).
    const SET_STATE: i32 = 0;
    const VOLATILE: i32 = 1;
    // SAFETY: libSystem keeps this task's send right alive for the process's lifetime.
    let task = unsafe { mach_task_self_ };
    let mut address = 0usize;
    // SAFETY: `address` is a live slot the kernel writes the new region's address into.
    let code = unsafe { vm_allocate(task, &raw mut address, BYTES, ANYWHERE_PURGABLE) };
    assert_eq!(code, 0, "vm_allocate");
    // SAFETY: the kernel just mapped `BYTES` writable bytes at `address`, owned by this test.
    unsafe { std::ptr::write_bytes(address as *mut u8, 0x5a, BYTES) };
    let mut state = VOLATILE;
    // SAFETY: `address` is the start of this test's purgeable region and `state` a live slot.
    let code = unsafe { vm_purgable_control(task, address, SET_STATE, &raw mut state) };
    let plain = read().expect("TASK_VM_INFO on this task");
    let mut raw = TaskVmInfo::default();
    let mut words = FULL_WORDS;
    // SAFETY: as `read_flavor`'s call: the flavor's writable layout and its word count.
    let raw_code = unsafe { task_info(task, TASK_VM_INFO, (&raw mut raw).cast(), &raw mut words) };
    let raw_resident = raw.purgeable_volatile_resident;
    let walked = read_purgeable().expect("TASK_VM_INFO_PURGEABLE on this task");
    // SAFETY: the region is this test's, unmapped once and not touched after.
    let released = unsafe { vm_deallocate(task, address, BYTES) };
    assert_eq!(code, 0, "vm_purgable_control");
    assert_eq!(released, 0, "vm_deallocate");
    assert_eq!(plain.purgeable, None);
    assert_eq!(
        (raw_code, raw_resident),
        (0, 0),
        "negative control: TASK_VM_INFO leaves it zero"
    );
    let counted = walked.purgeable.expect("the walk's words");
    assert!(
        counted.resident >= BYTES as u64 && counted.virtual_size >= BYTES as u64,
        "{counted:?}"
    );
    assert!(walked.ledger_purgeable_volatile >= BYTES as i64);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn self_vm_is_unsupported_off_macos() {
    assert_eq!(read(), Err(ReadError::Unsupported));
}
