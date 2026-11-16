// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scheduling observations, kept separate from a claim about the scheduler's cause.

/// Darwin process policy and lifetime CPU time, read together with the process counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheduling {
    /// Unix nice value, or the read's errno.
    pub nice: Result<i32, i32>,
    /// Target application bit from BSD info.
    pub application: bool,
    /// Target internal requested-background bit, not effective QoS.
    pub background_internal: bool,
    /// Target external requested-background bit, not effective QoS.
    pub background_external: bool,
    /// Target effective suppression flag.
    pub suppressed: bool,
    /// Darwin role (zero is unspecified), or the read's errno.
    pub role: Result<i32, i32>,
    /// Nanoseconds at each class in [`QOS_CLASSES`], or the read's errno.
    /// `None` means the conversion clock was unavailable, not zero CPU time.
    pub qos_cpu_ns: Option<Result<[u64; 7], i32>>,
}

/// The kernel's lifetime CPU-time class order. These are not P/E residency counters.
pub const QOS_CLASSES: [&str; 7] = [
    "default",
    "maintenance",
    "background",
    "utility",
    "legacy",
    "user_initiated",
    "user_interactive",
];

/// Read failure word, preserving an OS error distinct from an unsupported measurement.
#[must_use]
pub fn error_word(errno: i32) -> String {
    match errno {
        1 | 13 => "refused".into(),
        3 => "gone".into(),
        45 => "unsupported".into(),
        n => format!("os_error:{n}"),
    }
}
