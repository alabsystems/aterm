// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded observations of this process's published cumulative CPU and named threads.
//!
//! A cut brackets non-atomic read calls, not CPU-accounting epochs. Darwin's counters can omit
//! unpublished running-thread CPU, and a later read can publish work predating an earlier call.
//! Counter differences therefore describe published accounting growth, not CPU confined to the
//! read interval. Quantization bounds do not bound that publication lag. Thread names describe
//! observations, not ownership or a cause. Missing threads and names remain missing; this reader
//! neither distributes a process remainder nor computes shares.

use std::fmt;
use std::time::Instant;

#[cfg(target_os = "macos")]
#[path = "self_cpu_macos.rs"]
mod macos;

/// Maximum number of threads whose identities and counters one cut reads.
pub const MAX_THREADS: usize = 256;

/// One process generation within this boot, including its executable generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// This process's positive PID.
    pub pid: u32,
    /// Kernel start counter (`ri_proc_start_abstime`), compared only for equality.
    pub start: u64,
    /// Darwin's PID version, which changes at exec as well as process creation.
    pub version: i32,
}

/// Published cumulative CPU, with a one-sided bound on representation rounding only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuCounter {
    /// The observed published counter, nanoseconds; unpublished running CPU is absent.
    pub ns: u64,
    /// The counter's rounding interval is `[ns, ns + precision_ns]`, evaluated in `u128`.
    /// This describes the published value's representation, not accounting freshness or census
    /// completeness. It is not an upper bound on CPU consumed through the read call.
    pub precision_ns: u64,
}

/// One successfully identified thread at this cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadReading {
    /// Darwin's system-wide unique thread ID, never its reusable pthread or Mach port name.
    pub id: u64,
    /// Its observed, whole, bounded UTF-8 name; absent if empty, malformed, cut or unread.
    pub name: Option<String>,
    /// Published cumulative user plus system CPU; absent when unread or outside the numeric range.
    pub cpu: Option<CpuCounter>,
}

/// What one bounded census could observe, independent of how much CPU its threads used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// Number of Mach thread rights the kernel returned.
    pub listed: usize,
    /// Number inspected, at most [`MAX_THREADS`].
    pub inspected: usize,
    /// Inspected threads whose unique identity or CPU counter could not be read.
    pub unread: usize,
    /// Some listed threads were past the processing cap.
    pub capped: bool,
}

/// One self-only cut. Thread read calls lie between process read calls, whose accounting can
/// have different, unobserved publication epochs. Timestamps do not make these counters fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    /// The generation checked at both ends of the cut.
    pub process: ProcessIdentity,
    /// Monotonic time immediately before the cut, ns since the caller's epoch.
    pub begin_ns: u64,
    /// Monotonic time after its final generation check, on the same epoch.
    pub end_ns: u64,
    /// Published own process CPU before reading threads; no reaped-child CPU is included.
    pub process_before: CpuCounter,
    /// Published own process CPU after reading threads and releasing their rights and mapping.
    pub process_after: CpuCounter,
    /// Only threads whose unique identity could be read; missing CPU remains explicit.
    pub threads: Vec<ThreadReading>,
    /// Census coverage, including reads that produced no thread row.
    pub coverage: Coverage,
}

/// The operation whose native call refused or returned an unexpected record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    /// This process's cumulative resource usage.
    ProcessCpu,
    /// This task's thread-right array.
    ThreadList,
    /// A thread's unique identity.
    ThreadIdentity,
    /// A thread's CPU and name record.
    ThreadCpu,
    /// Returning one thread's send right.
    ReleaseThread,
    /// Returning the array's virtual-memory mapping.
    ReleaseList,
}

/// A whole cut that could not be observed. Partial per-thread reads use [`Coverage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// No self-thread CPU reader exists on this platform.
    Unsupported,
    /// The caller's epoch is in the future or its nanosecond offset exceeds `u64`.
    Clock,
    /// The kernel's timebase was unread or invalid; no 1:1 fallback is used.
    Timebase,
    /// The executable generation could not be read.
    Identity,
    /// The process generation changed during the cut.
    ProcessChanged,
    /// A cumulative counter or conversion does not fit in `u64`.
    CounterOverflow,
    /// A native call failed; Mach return codes and errno remain literal.
    Native {
        /// Which call failed.
        operation: Operation,
        /// Its native return code or errno.
        code: i32,
    },
    /// A native call returned a malformed or short record.
    Unexpected {
        /// Which record could not be interpreted.
        operation: Operation,
    },
}

impl ReadError {
    /// A stable diagnostic reason, without names, arguments or process contents.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Clock => "clock_unavailable",
            Self::Timebase => "timebase_unavailable",
            Self::Identity => "identity_unread",
            Self::ProcessChanged => "process_generation_changed",
            Self::CounterOverflow => "counter_overflow",
            Self::Native { operation, .. } | Self::Unexpected { operation } => match operation {
                Operation::ProcessCpu => "process_cpu_unread",
                Operation::ThreadList => "thread_list_unread",
                Operation::ThreadIdentity => "thread_identity_unread",
                Operation::ThreadCpu => "thread_cpu_unread",
                Operation::ReleaseThread => "thread_right_release_failed",
                Operation::ReleaseList => "thread_list_release_failed",
            },
        }
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())?;
        if let Self::Native { code, .. } = self {
            write!(f, " ({code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ReadError {}

/// Read this process alone, without starting a worker, opening a GUI API or changing policy.
///
/// # Errors
/// Returns an explicit error on unsupported platforms, a failed process/list/generation read,
/// invalid clock or counter conversion, or failed Mach cleanup. Thread read failures instead
/// remain visible in the successful cut's coverage and optional counters.
pub fn read(epoch: Instant) -> Result<Cut, ReadError> {
    #[cfg(target_os = "macos")]
    {
        macos::read(epoch)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = epoch;
        Err(ReadError::Unsupported)
    }
}

/// The self-reader's Mach layouts and flavors, checked against the SDK on both architectures.
#[cfg(target_os = "macos")]
pub const SDK_ROWS: &[(&str, i64)] = macos::SDK_ROWS;

#[cfg(any(target_os = "macos", test))]
fn thread_counter(user_ns: u64, system_ns: u64) -> Result<CpuCounter, ReadError> {
    Ok(CpuCounter {
        ns: user_ns
            .checked_add(system_ns)
            .ok_or(ReadError::CounterOverflow)?,
        // XNU's retrieve_thread_basic_info reads both time_value_t components at whole
        // microseconds; THREAD_EXTENDED_INFO converts them to ns. The published sum loses less
        // than 2 us to that representation. No bound on unpublished running CPU follows.
        precision_ns: 2_000,
    })
}

#[cfg(any(target_os = "macos", test))]
fn process_counter(
    user_ticks: u64,
    system_ticks: u64,
    numer: u32,
    denom: u32,
) -> Result<CpuCounter, ReadError> {
    if numer == 0 || denom == 0 {
        return Err(ReadError::Timebase);
    }
    let (numer, denom) = (u128::from(numer), u128::from(denom));
    let ns = (u128::from(user_ticks) + u128::from(system_ticks)) * numer / denom;
    // Two accumulated clock components, each bounded by one whole tick, plus the final
    // integer conversion's sub-nanosecond remainder. This does not bound publication lag.
    // Never assume a Rosetta 1:1 timebase.
    let precision = 2 * numer.div_ceil(denom) + 1;
    Ok(CpuCounter {
        ns: u64::try_from(ns).map_err(|_| ReadError::CounterOverflow)?,
        precision_ns: u64::try_from(precision).map_err(|_| ReadError::CounterOverflow)?,
    })
}

#[cfg(any(target_os = "macos", test))]
fn thread_name(bytes: &[u8; 64]) -> Option<String> {
    let end = bytes.iter().position(|b| *b == 0)?;
    if end == bytes.len() - 1 {
        return None;
    }
    let name = std::str::from_utf8(&bytes[..end]).ok()?;
    (!name.is_empty() && !name.chars().any(char::is_control)).then(|| name.to_owned())
}

#[cfg(any(target_os = "macos", test))]
fn stamp(epoch: Instant, now: Instant) -> Result<u64, ReadError> {
    let elapsed = now.checked_duration_since(epoch).ok_or(ReadError::Clock)?;
    u64::try_from(elapsed.as_nanos()).map_err(|_| ReadError::Clock)
}

#[cfg(any(target_os = "macos", test))]
fn observe_threads(
    ports: &[u32],
    mut read: impl FnMut(u32) -> Option<ThreadReading>,
) -> (Vec<ThreadReading>, Coverage) {
    let inspected = ports.len().min(MAX_THREADS);
    let mut coverage = Coverage {
        listed: ports.len(),
        inspected,
        unread: 0,
        capped: ports.len() > MAX_THREADS,
    };
    let mut threads = Vec::with_capacity(inspected);
    for &port in &ports[..inspected] {
        match read(port) {
            Some(thread) => {
                coverage.unread += usize::from(thread.cpu.is_none());
                threads.push(thread);
            }
            None => coverage.unread += 1,
        }
    }
    (threads, coverage)
}

#[cfg(any(target_os = "macos", test))]
mod resources {
    use super::{Operation, ReadError};

    pub(super) trait Release {
        fn thread(&self, port: u32) -> Result<(), i32>;
        fn array(&self, address: usize, bytes: usize) -> Result<(), i32>;
    }

    /// Owns every returned right, including entries beyond the read cap, and their array.
    pub(super) struct ThreadList<R: Release> {
        address: *mut u32,
        count: usize,
        release: R,
        owned: bool,
    }

    impl<R: Release> ThreadList<R> {
        /// # Safety
        /// `address` holds `count` initialized port names until `release.array` is called.
        /// Each entry is one owned send right; ownership of all entries and the array moves here.
        pub(super) unsafe fn new(address: *mut u32, count: usize, release: R) -> Self {
            Self {
                address,
                count,
                release,
                owned: true,
            }
        }

        pub(super) fn ports(&self) -> &[u32] {
            if self.count == 0 {
                return &[];
            }
            // SAFETY: the constructor's contract keeps the initialized array live until close.
            unsafe { std::slice::from_raw_parts(self.address, self.count) }
        }

        pub(super) fn close(mut self) -> Result<(), ReadError> {
            self.cleanup()
        }

        fn cleanup(&mut self) -> Result<(), ReadError> {
            if !self.owned {
                return Ok(());
            }
            self.owned = false;
            let mut first = None;
            for &port in self.ports() {
                if let Err(code) = self.release.thread(port) {
                    first.get_or_insert(ReadError::Native {
                        operation: Operation::ReleaseThread,
                        code,
                    });
                }
            }
            if self.count != 0 {
                if let Err(code) = self
                    .release
                    .array(self.address as usize, self.count * size_of::<u32>())
                {
                    first.get_or_insert(ReadError::Native {
                        operation: Operation::ReleaseList,
                        code,
                    });
                }
            }
            first.map_or(Ok(()), Err)
        }
    }

    impl<R: Release> Drop for ThreadList<R> {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }
}

#[cfg(test)]
#[path = "self_cpu_resources_tests.rs"]
mod resource_tests;

#[cfg(test)]
#[path = "self_cpu_tests.rs"]
mod tests;
