// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This task's virtual-memory ledgers (`task_info(mach_task_self(), TASK_VM_INFO)`): its
//! physical footprint (Activity Monitor's Memory), and the graphics and purgeable ledgers that
//! rev 3 of the record added, which say how much of the footprint the GPU driver's memory is
//! and how much of it the driver has marked volatile. The record grew by revision; a kernel that
//! answers fewer words than rev 3's is refused, never read short or as zeros. The layout is held
//! to the SDK on both architectures by `tests/proc_info_sdk.rs` ([`SDK_ROWS`]).
//!
//! The record's three `purgeable_volatile_*` words are filled only under the other flavor,
//! `TASK_VM_INFO_PURGEABLE` ([`read_purgeable`]): the kernel walks the task's whole map under
//! the map's read lock to count them, so that read is for a measurement's marks, never a
//! sampler's cadence. [`read`]'s flavor leaves them zero and its reading carries none.

use std::fmt;

/// `TASK_VM_INFO` (`mach/task_info.h`).
#[cfg(target_os = "macos")]
const TASK_VM_INFO: u32 = 22;

/// `TASK_VM_INFO_PURGEABLE` (`mach/task_info.h`): the same record, with the volatile purgeable
/// words counted by a walk of the map.
#[cfg(target_os = "macos")]
const TASK_VM_INFO_PURGEABLE: u32 = 23;

/// `TASK_VM_INFO_REV3_COUNT`: the words through the rev 3 ledgers, the least this reader takes.
pub const REV3_WORDS: u32 = 84;

/// `TASK_VM_INFO_COUNT` of the SDK this was written against (rev 7): the words asked for.
pub const FULL_WORDS: u32 = 93;

/// What one read answered, bytes. The `ledger_*` values are the kernel's signed ledgers, kept
/// signed as it keeps them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmReading {
    /// The words the kernel filled, at least [`REV3_WORDS`].
    pub words: u32,
    /// `phys_footprint`: the task's physical footprint.
    pub phys_footprint: u64,
    /// `internal`: anonymous memory resident.
    pub internal: u64,
    /// `compressed`: anonymous memory in the compressor. xnu's handler of
    /// `TASK_VM_INFO_PURGEABLE` may take the volatile purgeable compressed pages out of it, so
    /// compare it only between readings of one reader.
    pub compressed: u64,
    /// The volatile purgeable memory the map walk counted: present from [`read_purgeable`],
    /// `None` from [`read`], whose flavor never fills it.
    pub purgeable: Option<PurgeableVolatile>,
    /// `ledger_purgeable_nonvolatile`: nonvolatile purgeable memory charged to the footprint.
    pub ledger_purgeable_nonvolatile: i64,
    /// `ledger_purgeable_volatile`: volatile purgeable memory, outside the footprint.
    pub ledger_purgeable_volatile: i64,
    /// `ledger_tag_graphics_footprint`: memory tagged graphics, charged to the footprint.
    pub ledger_tag_graphics_footprint: i64,
    /// `ledger_tag_graphics_footprint_compressed`: its compressed part.
    pub ledger_tag_graphics_footprint_compressed: i64,
    /// `ledger_tag_graphics_nofootprint`: memory tagged graphics, outside the footprint.
    pub ledger_tag_graphics_nofootprint: i64,
    /// `ledger_tag_graphics_nofootprint_compressed`: its compressed part.
    pub ledger_tag_graphics_nofootprint_compressed: i64,
}

/// The record's volatile purgeable words, as `TASK_VM_INFO_PURGEABLE`'s map walk counts them,
/// bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PurgeableVolatile {
    /// `purgeable_volatile_pmap`: volatile purgeable memory the pmap holds.
    pub pmap: u64,
    /// `purgeable_volatile_resident`: volatile purgeable memory resident.
    pub resident: u64,
    /// `purgeable_volatile_virtual`: volatile purgeable memory mapped.
    pub virtual_size: u64,
}

/// A read that answered nothing usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// No such reader on this platform.
    Unsupported,
    /// `task_info` failed with this Mach return code.
    Native(i32),
    /// The kernel filled `words` words: fewer than rev 3's, or more than were asked for.
    Revision {
        /// The count it answered.
        words: u32,
    },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("unsupported"),
            Self::Native(code) => write!(f, "task_info failed ({code})"),
            Self::Revision { words } => write!(
                f,
                "TASK_VM_INFO answered {words} words, outside {REV3_WORDS}..={FULL_WORDS}"
            ),
        }
    }
}

impl std::error::Error for ReadError {}

/// Whether a record of `words` words carries every field this reader reads: at least rev 3's,
/// and no more than the buffer it was given.
///
/// # Errors
/// [`ReadError::Revision`] outside `REV3_WORDS..=FULL_WORDS`.
pub const fn accept(words: u32) -> Result<(), ReadError> {
    if words < REV3_WORDS || words > FULL_WORDS {
        Err(ReadError::Revision { words })
    } else {
        Ok(())
    }
}

/// `struct task_vm_info` through rev 7, under the header's `#pragma pack(4)`.
#[cfg(any(target_os = "macos", test))]
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64,
    min_address: u64,
    max_address: u64,
    ledger_phys_footprint_peak: i64,
    ledger_purgeable_nonvolatile: i64,
    ledger_purgeable_novolatile_compressed: i64,
    ledger_purgeable_volatile: i64,
    ledger_purgeable_volatile_compressed: i64,
    ledger_tag_network_nonvolatile: i64,
    ledger_tag_network_nonvolatile_compressed: i64,
    ledger_tag_network_volatile: i64,
    ledger_tag_network_volatile_compressed: i64,
    ledger_tag_media_footprint: i64,
    ledger_tag_media_footprint_compressed: i64,
    ledger_tag_media_nofootprint: i64,
    ledger_tag_media_nofootprint_compressed: i64,
    ledger_tag_graphics_footprint: i64,
    ledger_tag_graphics_footprint_compressed: i64,
    ledger_tag_graphics_nofootprint: i64,
    ledger_tag_graphics_nofootprint_compressed: i64,
    ledger_tag_neural_footprint: i64,
    ledger_tag_neural_footprint_compressed: i64,
    ledger_tag_neural_nofootprint: i64,
    ledger_tag_neural_nofootprint_compressed: i64,
    limit_bytes_remaining: u64,
    decompressions: i32,
    ledger_swapins: i64,
    ledger_tag_neural_nofootprint_total: i64,
    ledger_tag_neural_nofootprint_peak: i64,
}

#[cfg(any(target_os = "macos", test))]
const _: () = assert!(size_of::<TaskVmInfo>() == FULL_WORDS as usize * 4);

/// `info` as the kernel filled `words` words of it, or the refusal [`accept`] gives. The
/// volatile purgeable words are taken only where `purgeable` says the flavor that fills them
/// answered.
#[cfg(any(target_os = "macos", test))]
fn reading(info: &TaskVmInfo, words: u32, purgeable: bool) -> Result<VmReading, ReadError> {
    accept(words)?;
    // Each field is copied out of the packed record by value; none is borrowed.
    let i = *info;
    Ok(VmReading {
        words,
        phys_footprint: i.phys_footprint,
        internal: i.internal,
        compressed: i.compressed,
        purgeable: purgeable.then_some(PurgeableVolatile {
            pmap: i.purgeable_volatile_pmap,
            resident: i.purgeable_volatile_resident,
            virtual_size: i.purgeable_volatile_virtual,
        }),
        ledger_purgeable_nonvolatile: i.ledger_purgeable_nonvolatile,
        ledger_purgeable_volatile: i.ledger_purgeable_volatile,
        ledger_tag_graphics_footprint: i.ledger_tag_graphics_footprint,
        ledger_tag_graphics_footprint_compressed: i.ledger_tag_graphics_footprint_compressed,
        ledger_tag_graphics_nofootprint: i.ledger_tag_graphics_nofootprint,
        ledger_tag_graphics_nofootprint_compressed: i.ledger_tag_graphics_nofootprint_compressed,
    })
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    static mach_task_self_: u32;
    fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
}

/// Read this task's record now: one `task_info(TASK_VM_INFO)` call, no thread, no allocation.
/// Its reading carries no [`PurgeableVolatile`].
///
/// # Errors
/// [`ReadError::Unsupported`] off macOS; [`ReadError::Native`] when the call fails;
/// [`ReadError::Revision`] when the kernel answers fewer words than rev 3's.
pub fn read() -> Result<VmReading, ReadError> {
    #[cfg(target_os = "macos")]
    {
        read_flavor(TASK_VM_INFO)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ReadError::Unsupported)
    }
}

/// [`read`] with the volatile purgeable words counted: one `task_info(TASK_VM_INFO_PURGEABLE)`
/// call, which walks this task's whole map under the map's read lock.
///
/// # Errors
/// As [`read`]'s.
pub fn read_purgeable() -> Result<VmReading, ReadError> {
    #[cfg(target_os = "macos")]
    {
        read_flavor(TASK_VM_INFO_PURGEABLE)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ReadError::Unsupported)
    }
}

/// One `task_info` call of `flavor`, [`TASK_VM_INFO`] or [`TASK_VM_INFO_PURGEABLE`].
#[cfg(target_os = "macos")]
fn read_flavor(flavor: u32) -> Result<VmReading, ReadError> {
    let mut info = TaskVmInfo::default();
    let mut words = FULL_WORDS;
    // SAFETY: libSystem keeps this task's send right alive for the process's lifetime.
    let task = unsafe { mach_task_self_ };
    // SAFETY: `info` is both flavors' writable layout of `FULL_WORDS` natural_t words (held to
    // the SDK by `SDK_ROWS`), and `words` says so; the kernel writes at most that many and
    // answers how many it wrote.
    let code = unsafe { task_info(task, flavor, (&raw mut info).cast(), &raw mut words) };
    if code != 0 {
        return Err(ReadError::Native(code));
    }
    reading(&info, words, flavor == TASK_VM_INFO_PURGEABLE)
}

/// The record's flavors, counts, size and every offset [`read`] and [`read_purgeable`] take a
/// field from, checked
/// against the SDK's `mach/task_info.h` on both architectures.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("TASK_VM_INFO", TASK_VM_INFO as i64),
    ("TASK_VM_INFO_PURGEABLE", TASK_VM_INFO_PURGEABLE as i64),
    ("TASK_VM_INFO_COUNT", FULL_WORDS as i64),
    ("TASK_VM_INFO_REV3_COUNT", REV3_WORDS as i64),
    (
        "sizeof(struct task_vm_info)",
        size_of::<TaskVmInfo>() as i64,
    ),
    (
        "offsetof(struct task_vm_info, internal)",
        std::mem::offset_of!(TaskVmInfo, internal) as i64,
    ),
    (
        "offsetof(struct task_vm_info, purgeable_volatile_pmap)",
        std::mem::offset_of!(TaskVmInfo, purgeable_volatile_pmap) as i64,
    ),
    (
        "offsetof(struct task_vm_info, purgeable_volatile_resident)",
        std::mem::offset_of!(TaskVmInfo, purgeable_volatile_resident) as i64,
    ),
    (
        "offsetof(struct task_vm_info, purgeable_volatile_virtual)",
        std::mem::offset_of!(TaskVmInfo, purgeable_volatile_virtual) as i64,
    ),
    (
        "offsetof(struct task_vm_info, compressed)",
        std::mem::offset_of!(TaskVmInfo, compressed) as i64,
    ),
    (
        "offsetof(struct task_vm_info, phys_footprint)",
        std::mem::offset_of!(TaskVmInfo, phys_footprint) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_purgeable_nonvolatile)",
        std::mem::offset_of!(TaskVmInfo, ledger_purgeable_nonvolatile) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_purgeable_volatile)",
        std::mem::offset_of!(TaskVmInfo, ledger_purgeable_volatile) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_tag_graphics_footprint)",
        std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_footprint) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_tag_graphics_footprint_compressed)",
        std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_footprint_compressed) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_tag_graphics_nofootprint)",
        std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_tag_graphics_nofootprint_compressed)",
        std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint_compressed) as i64,
    ),
    (
        "offsetof(struct task_vm_info, ledger_tag_neural_nofootprint_peak)",
        std::mem::offset_of!(TaskVmInfo, ledger_tag_neural_nofootprint_peak) as i64,
    ),
];

#[cfg(test)]
#[path = "self_vm_tests.rs"]
mod tests;
