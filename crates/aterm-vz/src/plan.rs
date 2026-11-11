// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The VM plan (`docs/DESIGN-vz-guest-2026-10-23.md` §5, §9): what the helper builds,
//! validates and starts. The lab writes it; the helper resolves it against the framework's
//! limits and this host's cores at `Boot`, and prints the configuration it built.

use std::fmt::Write as _;
use std::path::PathBuf;

/// The kernel command line: hvc0 is the console the helper watches, the live disk is the
/// root, and `systemd.show_status=error` keeps systemd's status spinner off the console, so
/// a hung boot shows no console progress (fact 38; §10).
pub const COMMAND_LINE: &str = "console=hvc0 root=/dev/vda rw systemd.show_status=error";

/// The lane's CPUs, before the framework's and the host's clamp (§9).
pub const CPUS: u32 = 8;

/// The lane's memory: 16 GiB (§9).
pub const MEMORY: u64 = 16 << 30;

/// The host memory a lane needs besides its guest: 8 GiB for macOS (§9).
pub const HOST_MARGIN: u64 = 8 << 30;

/// The guest path `VZLinuxRosettaUnixSocketCachingOptions` points the translator at: the
/// symlink `rosettad.service`'s `ExecStartPost` makes (§6).
pub const ROSETTA_SOCKET: &str = "/run/rosettad/rosetta.sock";

/// The virtiofs tag of the per-run share: `repo`, `git` and `in`, all read-only (§7).
pub const ATERM_TAG: &str = "aterm";

/// One disk the guest sees, in order: the first is `/dev/vda`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk {
    /// A raw image file.
    pub path: PathBuf,
    /// Attached read-only (the bake's seed ISO).
    pub read_only: bool,
}

/// One directory of the `aterm` share: its name in the share and the host directory,
/// always read-only (§7: no share is writable).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShareEntry {
    /// `repo`, `git` or `in`.
    pub name: String,
    /// The host directory.
    pub path: PathBuf,
}

/// Everything the helper needs to build the VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The VM's name, which its MAC is derived from.
    pub name: String,
    /// The CPUs asked for, before the clamp ([`Limits::cpus`]).
    pub cpus: u32,
    /// The guest's memory.
    pub memory_bytes: u64,
    /// The arm64 `Image` (`crate::kernel::lay`).
    pub kernel: PathBuf,
    /// The initrd.
    pub initrd: PathBuf,
    /// The kernel command line.
    pub command_line: String,
    /// The disks, live first (`/dev/vda`), then a bake's seed.
    pub disks: Vec<Disk>,
    /// The NAT device's MAC ([`mac_for`]).
    pub mac: [u8; 6],
    /// The read-only boot share: the content-addressed directory of the agent (§6).
    pub boot_share: PathBuf,
    /// The `aterm` share's directories.
    pub shares: Vec<ShareEntry>,
    /// Whether to attach Rosetta's share, with its caching option at [`ROSETTA_SOCKET`].
    pub rosetta: bool,
    /// Where hvc0's bytes are written.
    pub console_log: PathBuf,
    /// The SHA-256 of the agent this checkout built, which the guest's `Hello` must name.
    pub agent_sha256: String,
}

/// What the framework and the host allow, read by the helper at `Boot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// `VZVirtualMachineConfiguration.maximumAllowedCPUCount`.
    pub max_cpus: u32,
    /// `maximumAllowedMemorySize`.
    pub max_memory: u64,
    /// This host's logical cores.
    pub host_cpus: u32,
    /// This host's memory.
    pub host_memory: u64,
}

impl Limits {
    /// The CPUs a plan asking `want` gets: `min(want, maximumAllowedCPUCount, the host's
    /// logical cores)`, and at least one (§9).
    #[must_use]
    pub fn cpus(&self, want: u32) -> u32 {
        want.min(self.max_cpus).min(self.host_cpus).max(1)
    }

    /// The NOT RUN words when `memory` cannot be given on this host: more than the framework
    /// allows, or a host with less than the guest plus [`HOST_MARGIN`], which would swap and
    /// so measure nothing (§9).
    #[must_use]
    pub fn memory_refusal(&self, memory: u64) -> Option<String> {
        if memory > self.max_memory {
            return Some(format!(
                "NOT RUN — the VM's {} exceed Virtualization.framework's maximumAllowedMemorySize, {}",
                crate::space::gib(memory),
                crate::space::gib(self.max_memory)
            ));
        }
        let need = memory.saturating_add(HOST_MARGIN);
        (self.host_memory < need).then(|| {
            format!(
                "NOT RUN — this host has {}; the VM's {} plus {} for macOS need {}: a lane that \
                 swaps the host measures nothing",
                crate::space::gib(self.host_memory),
                crate::space::gib(memory),
                crate::space::gib(HOST_MARGIN),
                crate::space::gib(need)
            )
        })
    }
}

/// A fixed, locally administered, unicast MAC for the VM `name`: FNV-1a/64 of the name, its
/// first octet's multicast bit cleared and its locally-administered bit set, so macOS's DHCP
/// server sees the same client at every boot and keeps one lease (§5).
#[must_use]
pub fn mac_for(name: &str) -> [u8; 6] {
    let h = aterm_hash::fnv1a_64(name.as_bytes()).to_be_bytes();
    let mut mac = [h[0], h[1], h[2], h[3], h[4], h[5]];
    mac[0] = (mac[0] & !0x01) | 0x02;
    mac
}

/// `mac` as `VZMACAddress` reads it: six lowercase hex pairs joined by `:`.
#[must_use]
pub fn mac_text(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

impl Plan {
    /// The configuration the helper builds from this plan under `limits`, one part per line,
    /// as its log and `status` print it (§5's table).
    #[must_use]
    pub fn config_text(&self, limits: &Limits) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "platform: VZGenericPlatformConfiguration");
        let _ = writeln!(
            out,
            "boot: VZLinuxBootLoader kernel={} initrd={} cmdline=\"{}\"",
            self.kernel.display(),
            self.initrd.display(),
            self.command_line
        );
        let _ = writeln!(
            out,
            "cpus: {} (asked {}, framework max {}, host {})",
            limits.cpus(self.cpus),
            self.cpus,
            limits.max_cpus,
            limits.host_cpus
        );
        let _ = writeln!(out, "memory: {}", crate::space::gib(self.memory_bytes));
        for (i, disk) in self.disks.iter().enumerate() {
            let dev = char::from(b'a'.saturating_add(u8::try_from(i).unwrap_or(25)));
            let _ = writeln!(
                out,
                "disk vd{dev}: VZVirtioBlockDeviceConfiguration {} {}",
                disk.path.display(),
                if disk.read_only {
                    "read-only"
                } else {
                    "writable"
                }
            );
        }
        let _ = writeln!(
            out,
            "network: VZVirtioNetworkDeviceConfiguration on VZNATNetworkDeviceAttachment, mac {}",
            mac_text(&self.mac)
        );
        let _ = writeln!(
            out,
            "channel: VZVirtioSocketDeviceConfiguration, listener on port {}",
            aterm_vz_agent::WATCH_PORT
        );
        let _ = writeln!(
            out,
            "share {}: VZSingleDirectoryShare {} read-only",
            crate::seed::BOOT_TAG,
            self.boot_share.display()
        );
        let _ = write!(out, "share {ATERM_TAG}: VZMultipleDirectoryShare [");
        for (i, e) in self.shares.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{}={}", e.name, e.path.display());
        }
        out.push_str("] read-only\n");
        if self.rosetta {
            let _ = writeln!(
                out,
                "share {}: VZLinuxRosettaDirectoryShare, caching at {ROSETTA_SOCKET}",
                crate::seed::ROSETTA_TAG
            );
        }
        let _ = writeln!(
            out,
            "console: VZVirtioConsoleDeviceSerialPortConfiguration to {}, no input",
            self.console_log.display()
        );
        let _ = writeln!(out, "entropy: VZVirtioEntropyDeviceConfiguration");
        let _ = writeln!(
            out,
            "absent: graphics, keyboard, pointing device, audio, balloon"
        );
        let _ = writeln!(out, "agent: {}", self.agent_sha256);
        out
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
