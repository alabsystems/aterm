// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `proc_pid_rusage`'s record as the process readers take it: `RUSAGE_INFO_V6` where the kernel
//! has that flavor (macOS 12 and later), else `RUSAGE_INFO_V4` read into the same record's
//! prefix, whose layout V6 begins with. A kernel that predates a flavor refuses the whole call
//! with `EINVAL` (xnu `bsd/kern/kern_resource.c`), so the first such refusal decides the flavor
//! for the process's lifetime ([`Flavor`]).

#[cfg(any(target_os = "macos", test))]
use std::ffi::c_int;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::{AtomicU8, Ordering};

/// `RUSAGE_INFO_V4` (`sys/resource.h`).
#[cfg(any(target_os = "macos", test))]
pub(crate) const RUSAGE_INFO_V4: c_int = 4;

/// `RUSAGE_INFO_V6` (`sys/resource.h`).
#[cfg(any(target_os = "macos", test))]
pub(crate) const RUSAGE_INFO_V6: c_int = 6;

/// `EINVAL`, the answer of a kernel that has no such flavor.
#[cfg(any(target_os = "macos", test))]
const EINVAL: i32 = 22;

/// `struct rusage_info_v6` (`sys/resource.h`, 464 bytes). The fields the readers read carry the
/// SDK's names; each run of the others keeps its place as an array. A V4 read fills the first
/// 296 bytes, through `ri_runnable_time`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RusageV6 {
    /// `ri_uuid`.
    _uuid: [u8; 16],
    pub(crate) ri_user_time: u64,
    pub(crate) ri_system_time: u64,
    /// `ri_pkg_idle_wkups`.
    _pkg_idle_wkups: u64,
    pub(crate) ri_interrupt_wkups: u64,
    pub(crate) ri_pageins: u64,
    /// `ri_wired_size`.
    _wired_size: u64,
    pub(crate) ri_resident_size: u64,
    pub(crate) ri_phys_footprint: u64,
    pub(crate) ri_proc_start_abstime: u64,
    /// `ri_proc_exit_abstime`.
    _proc_exit_abstime: u64,
    pub(crate) ri_child_user_time: u64,
    pub(crate) ri_child_system_time: u64,
    /// `ri_child_pkg_idle_wkups` through `ri_child_elapsed_abstime`.
    _child_wakeups_to_elapsed: [u64; 4],
    pub(crate) ri_diskio_bytesread: u64,
    pub(crate) ri_diskio_byteswritten: u64,
    pub(crate) ri_cpu_time_qos_default: u64,
    pub(crate) ri_cpu_time_qos_maintenance: u64,
    pub(crate) ri_cpu_time_qos_background: u64,
    pub(crate) ri_cpu_time_qos_utility: u64,
    pub(crate) ri_cpu_time_qos_legacy: u64,
    pub(crate) ri_cpu_time_qos_user_initiated: u64,
    pub(crate) ri_cpu_time_qos_user_interactive: u64,
    /// `ri_billed_system_time` and `ri_serviced_system_time`.
    _billed_serviced: [u64; 2],
    pub(crate) ri_logical_writes: u64,
    pub(crate) ri_lifetime_max_phys_footprint: u64,
    pub(crate) ri_instructions: u64,
    /// `ri_cycles` through `ri_interval_max_phys_footprint`.
    _cycles_to_interval_max: [u64; 4],
    pub(crate) ri_runnable_time: u64,
    /// `ri_flags` (V5).
    _flags: u64,
    pub(crate) ri_user_ptime: u64,
    pub(crate) ri_system_ptime: u64,
    pub(crate) ri_pinstructions: u64,
    /// `ri_pcycles`.
    _pcycles: u64,
    pub(crate) ri_energy_nj: u64,
    /// `ri_penergy_nj` through `ri_page_cache_hits`, then `ri_reserved`.
    _after_energy: [u64; 15],
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<RusageV6>() == 464);
#[cfg(target_os = "macos")]
const _: () =
    assert!(std::mem::offset_of!(RusageV6, _flags) == std::mem::size_of::<libc::rusage_info_v4>());

/// One process's usage as read: the record, and whether the kernel filled it as V6.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Usage {
    /// The record; past `ri_runnable_time` only where [`Self::v6`].
    pub(crate) ri: RusageV6,
    /// Whether the kernel answered `RUSAGE_INFO_V6`.
    pub(crate) v6: bool,
}

#[cfg(target_os = "macos")]
impl Usage {
    /// The energy its threads used since it started, nanojoules (`ri_energy_nj`); `None` from a
    /// V4 read.
    pub(crate) fn energy_nj(&self) -> Option<u64> {
        self.v6.then_some(self.ri.ri_energy_nj)
    }

    /// Its user plus system time on performance cores in ticks (`ri_user_ptime +
    /// ri_system_ptime`), the instructions it retired (`ri_instructions`) and those it retired
    /// there (`ri_pinstructions`); `None` from a V4 read.
    pub(crate) fn performance_cores(&self) -> Option<(u64, u64, u64)> {
        self.v6.then(|| {
            (
                self.ri
                    .ri_user_ptime
                    .saturating_add(self.ri.ri_system_ptime),
                self.ri.ri_instructions,
                self.ri.ri_pinstructions,
            )
        })
    }
}

/// The flavor this kernel answers, learned from the first read that tells: unknown until a V6
/// read is answered (V6) or refused with `EINVAL` (V4); any other refusal (a gone process,
/// another user's) tells nothing.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
pub(crate) struct Flavor(AtomicU8);

#[cfg(any(target_os = "macos", test))]
const UNKNOWN: u8 = 0;
#[cfg(any(target_os = "macos", test))]
const V6: u8 = 6;
#[cfg(any(target_os = "macos", test))]
const V4: u8 = 4;

#[cfg(any(target_os = "macos", test))]
impl Flavor {
    /// Not yet known.
    pub(crate) const fn new() -> Self {
        Self(AtomicU8::new(UNKNOWN))
    }

    /// One read into `record` through `call(flavor, record)` (`Ok` or the errno): V6 unless this
    /// kernel refused it, then V4 into the same record. `Ok(true)` where the kernel answered V6.
    ///
    /// # Errors
    /// The errno of the read that failed.
    pub(crate) fn read<R>(
        &self,
        record: &mut R,
        mut call: impl FnMut(c_int, &mut R) -> Result<(), i32>,
    ) -> Result<bool, i32> {
        if self.0.load(Ordering::Relaxed) != V4 {
            match call(RUSAGE_INFO_V6, record) {
                Ok(()) => {
                    self.0.store(V6, Ordering::Relaxed);
                    return Ok(true);
                }
                Err(EINVAL) if self.0.load(Ordering::Relaxed) == UNKNOWN => {
                    self.0.store(V4, Ordering::Relaxed);
                }
                Err(errno) => return Err(errno),
            }
        }
        call(RUSAGE_INFO_V4, record).map(|()| false)
    }
}

/// The flavors and the record's layout this module declares, held to the SDK headers by
/// `tests/proc_info_sdk.rs`.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = &[
    ("RUSAGE_INFO_V4", RUSAGE_INFO_V4 as i64),
    ("RUSAGE_INFO_V6", RUSAGE_INFO_V6 as i64),
    (
        "sizeof(struct rusage_info_v6)",
        std::mem::size_of::<RusageV6>() as i64,
    ),
    (
        "sizeof(struct rusage_info_v4)",
        std::mem::size_of::<libc::rusage_info_v4>() as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_user_time)",
        std::mem::offset_of!(RusageV6, ri_user_time) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_system_time)",
        std::mem::offset_of!(RusageV6, ri_system_time) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_interrupt_wkups)",
        std::mem::offset_of!(RusageV6, ri_interrupt_wkups) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_pageins)",
        std::mem::offset_of!(RusageV6, ri_pageins) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_resident_size)",
        std::mem::offset_of!(RusageV6, ri_resident_size) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_phys_footprint)",
        std::mem::offset_of!(RusageV6, ri_phys_footprint) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_proc_start_abstime)",
        std::mem::offset_of!(RusageV6, ri_proc_start_abstime) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_child_user_time)",
        std::mem::offset_of!(RusageV6, ri_child_user_time) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_child_system_time)",
        std::mem::offset_of!(RusageV6, ri_child_system_time) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_diskio_bytesread)",
        std::mem::offset_of!(RusageV6, ri_diskio_bytesread) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_diskio_byteswritten)",
        std::mem::offset_of!(RusageV6, ri_diskio_byteswritten) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_default)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_default) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_maintenance)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_maintenance) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_background)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_background) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_utility)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_utility) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_legacy)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_legacy) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_user_initiated)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_user_initiated) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_cpu_time_qos_user_interactive)",
        std::mem::offset_of!(RusageV6, ri_cpu_time_qos_user_interactive) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_logical_writes)",
        std::mem::offset_of!(RusageV6, ri_logical_writes) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_lifetime_max_phys_footprint)",
        std::mem::offset_of!(RusageV6, ri_lifetime_max_phys_footprint) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_instructions)",
        std::mem::offset_of!(RusageV6, ri_instructions) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_runnable_time)",
        std::mem::offset_of!(RusageV6, ri_runnable_time) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_flags)",
        std::mem::offset_of!(RusageV6, _flags) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_user_ptime)",
        std::mem::offset_of!(RusageV6, ri_user_ptime) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_system_ptime)",
        std::mem::offset_of!(RusageV6, ri_system_ptime) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_pinstructions)",
        std::mem::offset_of!(RusageV6, ri_pinstructions) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_energy_nj)",
        std::mem::offset_of!(RusageV6, ri_energy_nj) as i64,
    ),
    (
        "offsetof(struct rusage_info_v6, ri_penergy_nj)",
        std::mem::offset_of!(RusageV6, _after_energy) as i64,
    ),
];

#[cfg(test)]
#[path = "rusage_tests.rs"]
mod tests;
