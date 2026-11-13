// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! What this machine's readers can say, measured on it (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §4.2, §4.6, §16).

use std::fmt;

/// Why a figure is unavailable on this machine (design §4.2's `Why`, the subset the readers here
/// can name).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// The OS has no source for it (the load average on Windows; purgeable space off macOS).
    Unsupported,
    /// The OS has a public source this build does not read (macOS's thermal notify level).
    NotRead,
    /// The OS can, with a feature that is switched off here: the name is the feature
    /// (`not_enabled:psi`, `not_enabled:zswap`).
    NotEnabled(&'static str),
    /// Only with a privilege aterm never asks for: the name is the privilege
    /// (`needs_privilege:cap_perfmon`).
    NeedsPrivilege(&'static str),
    /// The device is runtime-suspended and reading it would wake it, so it is not read: known idle.
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
    /// Read from a source that measures it (a kernel counter, the OS's own figure).
    Yes,
    /// Read, but the figure is a heuristic (Linux's commit limit under overcommit, a throttle level
    /// from trip points, a process list a `hidepid` mount cuts).
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

/// What the machine's capacity figures are computed against (design §4.3 `CapSource`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CapSource {
    /// The whole host.
    #[default]
    Host,
    /// A Linux cgroup v2 limit on this process (`cpu.max`, `memory.max`): inside a container
    /// `/proc` shows the host, the cgroup the ceiling.
    CgroupLimit,
}

impl CapSource {
    /// The wire word: `host`, `cgroup_limit`.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            CapSource::Host => "host",
            CapSource::CgroupLimit => "cgroup_limit",
        }
    }
}

/// What this machine's readers can say, measured on it ([`fn@crate::caps`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caps {
    /// The OS the readers were built for: `macos`, `linux`, `windows`, `other`.
    pub os: &'static str,
    /// Every logical core's busy and idle ticks.
    pub per_core: Cap,
    /// The cores grouped by performance level (macOS `hw.perflevelN`, Linux
    /// `cpu_core`/`cpu_atom` PMUs or `cpu_capacity` per core design, Windows CPU Sets'
    /// `EfficiencyClass`).
    pub core_kinds: Cap,
    /// Which cores the OS took away (Linux offline CPUs, Windows parked CPU Sets).
    pub cores_withdrawn: Cap,
    /// Memory by the OS's own classes: `yes` where they partition physical memory exactly (macOS),
    /// `estimate` where a remainder is not classified (Linux, Windows; design §9.6).
    pub memory_classes: Cap,
    /// The kernel's memory pressure (macOS's level, Linux PSI, Windows' commit charge against its
    /// limit with the low-memory notification).
    pub pressure: Cap,
    /// Commit charge and limit (Windows exact; Linux `Committed_AS` against an advisory
    /// `CommitLimit`; macOS has none).
    pub commit: Cap,
    /// Compressed memory (macOS's compressor, Linux zswap, Windows' compression store).
    pub compressor: Cap,
    /// Swap in use and allocated.
    pub swap: Cap,
    /// The GPU's device-wide use (Windows: the busiest hardware adapter's busiest engine, from
    /// the `GPU Engine` counters).
    pub gpu_device: Cap,
    /// GPU use per process (Windows: each `GPU Engine` instance's running time, every user's).
    pub gpu_per_proc: Cap,
    /// CPU time of other users' processes.
    pub proc_cpu_others: Cap,
    /// Memory of other users' processes.
    pub proc_memory_others: Cap,
    /// The throttle level (`none`, `light`, `heavy`, `severe`); `estimate` on Windows, where it
    /// is a banding of the thermal zones' passive limit and a machine without a zone has none.
    pub throttle: Cap,
    /// A volume's size and free space.
    pub volume: Cap,
    /// The kernel's resource coalitions, every user's ([`crate::coalitions`]): `yes` on macOS once
    /// [`crate::coalitions::self_check`] passes.
    pub coalitions: Cap,
    /// Rows this OS's readers add to the table above, each `(name, cap, what it measures)`, in
    /// the order they print: Windows' rows for the readers the table has no column for (physical
    /// memory, paging, whose a process is, its I/O and state, a process tree's memory, the
    /// machine's words, the network links, the TCP counts, device throughput, load). Empty on
    /// macOS and Linux, whose report is the table above.
    pub more: Vec<(&'static str, Cap, &'static str)>,
    /// What capacity figures are computed against.
    pub capacity: CapSource,
}

impl Caps {
    /// Every row as `(name, word)`, in the order the struct declares them ([`Caps::more`]'s
    /// before `capacity`): `("per_core", "yes")`,
    /// `("pressure", "unavailable(not_enabled:psi)")`.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        self.described_rows()
            .into_iter()
            .map(|(name, word, _)| (name, word))
            .collect()
    }

    /// Every row as `(name, word, meaning)`: [`Caps::rows`] with what each row measures, in words
    /// for a reader who has not seen this struct (`aterm help sys machine` prints them).
    #[must_use]
    pub fn described_rows(&self) -> Vec<(&'static str, String, &'static str)> {
        let cap =
            |name: &'static str, c: Cap, meaning: &'static str| (name, c.to_string(), meaning);
        let mut rows = vec![
            (
                "os",
                self.os.to_owned(),
                "the OS these readers were built for",
            ),
            cap(
                "per_core",
                self.per_core,
                "each logical core's busy and idle time",
            ),
            cap(
                "core_kinds",
                self.core_kinds,
                "the cores grouped by performance level",
            ),
            cap(
                "cores_withdrawn",
                self.cores_withdrawn,
                "which cores the OS took away (offline, parked)",
            ),
            cap(
                "memory_classes",
                self.memory_classes,
                "memory by the OS's classes (yes: they add up to RAM)",
            ),
            cap("pressure", self.pressure, "the kernel's memory pressure"),
            cap("commit", self.commit, "the commit charge and its limit"),
            cap(
                "compressor",
                self.compressor,
                "compressed memory (macOS's compressor, zswap, Windows' compression store)",
            ),
            cap("swap", self.swap, "swap in use and allocated"),
            cap("gpu_device", self.gpu_device, "the GPU's device-wide use"),
            cap("gpu_per_proc", self.gpu_per_proc, "GPU use per process"),
            cap(
                "proc_cpu_others",
                self.proc_cpu_others,
                "other users' processes' CPU time",
            ),
            cap(
                "proc_memory_others",
                self.proc_memory_others,
                "other users' processes' memory",
            ),
            cap("throttle", self.throttle, "the throttle level"),
            cap("volume", self.volume, "a volume's size and free space"),
            cap(
                "coalitions",
                self.coalitions,
                "every user's resource coalitions (why's split)",
            ),
        ];
        rows.extend(
            self.more
                .iter()
                .map(|&(name, c, meaning)| cap(name, c, meaning)),
        );
        rows.push((
            "capacity",
            self.capacity.word().to_owned(),
            "what capacity is counted against: host or cgroup_limit",
        ));
        rows
    }

    /// Every row unavailable as [`Why::Unsupported`]: a platform with no readers here.
    #[inline]
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
            coalitions: no,
            more: Vec::new(),
            capacity: CapSource::Host,
        }
    }
}

/// The GPU row from a reading ([`crate::gpu`]): `yes` when a device answered with its use or is
/// known idle because it sleeps, else why not; no device at all is [`Why::Absent`].
#[inline]
#[must_use]
pub const fn gpu_cap(reading: Option<&crate::GpuReading>) -> Cap {
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
