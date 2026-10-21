// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THIS MACHINE CAN SAY, measured on it (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §4.2, §4.6, §16).
//!
//! * [`Why`] — the reason a figure is unavailable. A figure the OS cannot
//!   give is never a zero standing in for "unknown": it is `None` with a
//!   `Why` beside it where the caller needs to say why.
//! * [`Cap`] — one row of the report: `yes`, `estimate`, or
//!   `unavailable(<why>)`.
//! * [`Caps`] — the report [`crate::caps`] measures on the running machine,
//!   one [`Cap`] per signal the readers here answer, in the words
//!   `aterm help sys` and `aterm ctl sys` print ([`Caps::rows`]).

use std::fmt;

/// Why a figure is unavailable on this machine (design §4.2's `Why`, the
/// subset the readers here can name).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// The OS has no source for it (the thermal level on Windows; purgeable
    /// space off macOS).
    Unsupported,
    /// The OS has a public source this build does not read (Windows' GPU
    /// engine counters, which cost more per read than the whole sampler;
    /// macOS's thermal notify level).
    NotRead,
    /// The OS can, with a feature that is switched off here: the name is the
    /// feature (`not_enabled:psi`, `not_enabled:zswap`).
    NotEnabled(&'static str),
    /// Only with a privilege aterm never asks for: the name is the privilege
    /// (`needs_privilege:cap_perfmon`).
    NeedsPrivilege(&'static str),
    /// The device is runtime-suspended and reading it would wake it, so it
    /// is not read: known idle.
    Asleep,
    /// This machine has no such device (no GPU, no thermal zone).
    Absent,
}

impl fmt::Display for Why {
    /// The wire word: `unsupported`, `not_read`, `not_enabled:psi`,
    /// `needs_privilege:cap_perfmon`, `asleep`, `absent`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Why::Unsupported => f.write_str("unsupported"),
            Why::NotRead => f.write_str("not_read"),
            Why::NotEnabled(what) => write!(f, "not_enabled:{what}"),
            Why::NeedsPrivilege(what) => write!(f, "needs_privilege:{what}"),
            Why::Asleep => f.write_str("asleep"),
            Why::Absent => f.write_str("absent"),
        }
    }
}

/// One capability: measured exactly, estimated, or unavailable and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cap {
    /// Read from a source that measures it (a kernel counter, the OS's own
    /// figure).
    Yes,
    /// Read, but the figure is a heuristic (Linux's commit limit under
    /// overcommit, a throttle level from trip points, a process list a
    /// `hidepid` mount cuts).
    Estimate,
    /// Not available here.
    No(Why),
}

impl fmt::Display for Cap {
    /// `yes`, `estimate`, or `unavailable(<why>)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cap::Yes => f.write_str("yes"),
            Cap::Estimate => f.write_str("estimate"),
            Cap::No(why) => write!(f, "unavailable({why})"),
        }
    }
}

/// What the machine's capacity figures are computed against (design §4.3
/// `CapSource`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CapSource {
    /// The whole host.
    #[default]
    Host,
    /// A Linux cgroup v2 limit on this process (`cpu.max`, `memory.max`):
    /// inside a container `/proc` shows the host, the cgroup the ceiling.
    CgroupLimit,
}

impl CapSource {
    /// The wire word: `host`, `cgroup_limit`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            CapSource::Host => "host",
            CapSource::CgroupLimit => "cgroup_limit",
        }
    }
}

/// What this machine's readers can say, measured on it ([`crate::caps`]).
/// Each row names its source per OS in the design's capability matrix (§16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    /// The OS the readers were built for: `macos`, `linux`, `windows`, `other`.
    pub os: &'static str,
    /// Every logical core's busy and idle ticks.
    pub per_core: Cap,
    /// The cores grouped by performance level (macOS `hw.perflevelN`, Linux
    /// `cpu_core`/`cpu_atom` PMUs or `cpu_capacity`, Windows CPU Sets'
    /// `EfficiencyClass`).
    pub core_kinds: Cap,
    /// Which cores the OS took away (Linux offline CPUs, Windows parked CPU
    /// Sets).
    pub cores_withdrawn: Cap,
    /// Memory by the OS's own classes: `yes` where they partition physical
    /// memory exactly (macOS), `estimate` where a remainder is not classified
    /// (Linux, Windows; design §9.6).
    pub memory_classes: Cap,
    /// The kernel's memory pressure (macOS's level, Linux PSI, Windows'
    /// commit charge against its limit with the low-memory notification).
    pub pressure: Cap,
    /// Commit charge and limit (Windows exact; Linux `Committed_AS` against
    /// an advisory `CommitLimit`; macOS has none).
    pub commit: Cap,
    /// Compressed memory (macOS's compressor, Linux zswap).
    pub compressor: Cap,
    /// Swap in use and allocated.
    pub swap: Cap,
    /// The GPU's device-wide use.
    pub gpu_device: Cap,
    /// GPU use per process.
    pub gpu_per_proc: Cap,
    /// CPU time of other users' processes.
    pub proc_cpu_others: Cap,
    /// Memory of other users' processes.
    pub proc_memory_others: Cap,
    /// The throttle level (`none`, `light`, `heavy`, `severe`).
    pub throttle: Cap,
    /// A volume's size and free space.
    pub volume: Cap,
    /// What capacity figures are computed against.
    pub capacity: CapSource,
}

impl Caps {
    /// Every row as `(name, word)`, in the order the struct declares them:
    /// `("per_core", "yes")`, `("pressure", "unavailable(not_enabled:psi)")`.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let cap = |name: &'static str, c: Cap| (name, c.to_string());
        vec![
            ("os", self.os.to_owned()),
            cap("per_core", self.per_core),
            cap("core_kinds", self.core_kinds),
            cap("cores_withdrawn", self.cores_withdrawn),
            cap("memory_classes", self.memory_classes),
            cap("pressure", self.pressure),
            cap("commit", self.commit),
            cap("compressor", self.compressor),
            cap("swap", self.swap),
            cap("gpu_device", self.gpu_device),
            cap("gpu_per_proc", self.gpu_per_proc),
            cap("proc_cpu_others", self.proc_cpu_others),
            cap("proc_memory_others", self.proc_memory_others),
            cap("throttle", self.throttle),
            cap("volume", self.volume),
            ("capacity", self.capacity.word().to_owned()),
        ]
    }

    /// Every row unavailable as [`Why::Unsupported`]: a platform with no
    /// readers here.
    #[must_use]
    pub const fn unsupported(os: &'static str) -> Self {
        let no = Cap::No(Why::Unsupported);
        Caps {
            os,
            per_core: no,
            core_kinds: no,
            cores_withdrawn: no,
            memory_classes: no,
            pressure: no,
            commit: no,
            compressor: no,
            swap: no,
            gpu_device: no,
            gpu_per_proc: no,
            proc_cpu_others: no,
            proc_memory_others: no,
            throttle: no,
            volume: no,
            capacity: CapSource::Host,
        }
    }
}

/// The GPU row from a reading ([`crate::gpu`]): `yes` when a device answered
/// with its use or is known idle because it sleeps, else why not; no device
/// at all is [`Why::Absent`].
#[must_use]
pub fn gpu_cap(reading: Option<&crate::GpuReading>) -> Cap {
    match reading {
        None => Cap::No(Why::Absent),
        Some(g) if g.device_pct.is_some() => Cap::Yes,
        Some(g) => match g.unread {
            Some(Why::Asleep) => Cap::Yes,
            Some(why) => Cap::No(why),
            None => Cap::No(Why::Unsupported),
        },
    }
}
