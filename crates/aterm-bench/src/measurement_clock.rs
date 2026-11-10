// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kernel clocks and retired-work counters shared by benchmark harnesses.

/// This thread's CPU time in nanoseconds, or None if the kernel refuses its clock.
#[cfg(unix)]
pub fn thread_cpu_ns() -> Option<u64> {
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: stamp is initialized and writable for this one call.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut stamp) } != 0 {
        return None;
    }
    Some(
        u64::try_from(stamp.tv_sec).unwrap_or(0) * 1_000_000_000
            + u64::try_from(stamp.tv_nsec).unwrap_or(0),
    )
}

/// Without a thread CPU clock, return elapsed wall time; NAME identifies that fallback.
#[cfg(not(unix))]
pub fn thread_cpu_ns() -> Option<u64> {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    Some(
        u64::try_from(
            ORIGIN
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX),
    )
}

/// Which clock thread_cpu_ns returns on this platform.
pub const NAME: &str = if cfg!(unix) { "thread_cpu" } else { "wall" };

/// The process's kernel counters, cumulative: CPU time in nanoseconds, cycles and
/// instructions retired, each in all and on performance cores alone.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[must_use]
pub struct Counters {
    pub cpu_ns: f64,
    pub pcpu_ns: f64,
    pub cycles: u64,
    pub pcycles: u64,
    pub instructions: u64,
    pub pinstructions: u64,
}

impl Counters {
    pub fn since(self, then: Self) -> Self {
        Self {
            cpu_ns: self.cpu_ns - then.cpu_ns,
            pcpu_ns: self.pcpu_ns - then.pcpu_ns,
            cycles: self.cycles.saturating_sub(then.cycles),
            pcycles: self.pcycles.saturating_sub(then.pcycles),
            instructions: self.instructions.saturating_sub(then.instructions),
            pinstructions: self.pinstructions.saturating_sub(then.pinstructions),
        }
    }
}

/// `struct rusage_info_v6` (`<sys/resource.h>`), which libc does not declare; the fields this
/// harness reads are named, the others kept as runs of their place.
#[cfg(target_os = "macos")]
#[repr(C)]
struct RusageV6 {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    /// `ri_pkg_idle_wkups` through `ri_lifetime_max_phys_footprint`.
    _before_instructions: [u64; 27],
    instructions: u64,
    cycles: u64,
    /// `ri_billed_energy` through `ri_flags`.
    _before_ptime: [u64; 5],
    user_ptime: u64,
    system_ptime: u64,
    pinstructions: u64,
    pcycles: u64,
    /// `ri_energy_nj` through `ri_page_cache_hits`, then `ri_reserved`.
    _after_pcycles: [u64; 16],
}

/// `RUSAGE_INFO_V6`.
#[cfg(target_os = "macos")]
const RUSAGE_INFO_V6: libc::c_int = 6;

/// Nanoseconds per Mach tick, the unit of `rusage_info`'s times.
#[cfg(target_os = "macos")]
fn ns_per_tick() -> Option<f64> {
    static TICK: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    *TICK.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `tb` is writable for one `mach_timebase_info`.
        let rc = unsafe { libc::mach_timebase_info(&mut tb) };
        (rc == 0 && tb.denom != 0).then(|| f64::from(tb.numer) / f64::from(tb.denom))
    })
}

/// The process's counters, or `None` where the kernel does not count them by core type.
#[cfg(target_os = "macos")]
pub fn counters() -> Option<Counters> {
    let tick = ns_per_tick()?;
    let mut info = std::mem::MaybeUninit::<RusageV6>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: `info` is writable for one `rusage_info_v6`, the flavor asked for, whose
    // layout `RusageV6` repeats field for field.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            RUSAGE_INFO_V6,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: zeroed at creation, so initialized whatever the call wrote.
    let info = unsafe { info.assume_init() };
    Some(Counters {
        cpu_ns: (info.user_time + info.system_time) as f64 * tick,
        pcpu_ns: (info.user_ptime + info.system_ptime) as f64 * tick,
        cycles: info.cycles,
        pcycles: info.pcycles,
        instructions: info.instructions,
        pinstructions: info.pinstructions,
    })
    .filter(|c| c.instructions > 0 && c.cycles > 0)
}

#[cfg(not(target_os = "macos"))]
pub fn counters() -> Option<Counters> {
    None
}

/// Declare this thread user-interactive, the class macOS runs on performance cores first:
/// whether it took.
#[cfg(target_os = "macos")]
pub fn declare_interactive() -> bool {
    // SAFETY: sets the calling thread's own QoS class; takes no pointers.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) == 0
    }
}

#[cfg(not(target_os = "macos"))]
pub const fn declare_interactive() -> bool {
    false
}
