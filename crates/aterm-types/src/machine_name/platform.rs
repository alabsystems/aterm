// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `PlatformId` port (`docs/DESIGN-machine-names-2026-11-04.md` §3.2, §6.3): what the
//! platform says of this machine for its name, read only through one injected value, so a test
//! or a simulated node gets fixed made-up answers and never the host's.
//!
//! The platform id and the boot id are read only to derive a record's anchor and boot digest;
//! neither is stored or sent. The container markers and [`is_ephemeral`] decide whether a node
//! spends numbers at all.

use std::ffi::OsString;

/// The development seam ([`crate::dev_seam!`]) that replaces [`PlatformId::platform_id`]'s host
/// read, so lab instances on one Mac can be distinct machines: its value, trimmed, or no platform
/// id when it is set but blank.
pub const PLATFORM_ID_SEAM: &str = "ATERM_DEBUG_PLATFORM_ID";

/// The development seam that replaces [`PlatformId::boot_id`]'s host read, as
/// [`PLATFORM_ID_SEAM`] replaces the platform id's, so two instances given one platform id can be
/// running in two boots at once.
pub const BOOT_ID_SEAM: &str = "ATERM_DEBUG_BOOT_ID";

/// Files whose presence says a Linux node runs inside a container (systemd's, Docker's and
/// Podman's), as `systemd-detect-virt --container` reads them.
pub const CONTAINER_MARKERS: [&str; 3] = [
    "/run/systemd/container",
    "/.dockerenv",
    "/run/.containerenv",
];

/// Where a machine's platform reads come from: the host, or fixed answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformId {
    /// This host: Apple's `gethostuuid` (its IOPlatformUUID) or a root-owned `/etc/machine-id`
    /// (`aterm_uds::process::host_id`), its boot session (`kern.bootsessionuuid`,
    /// `/proc/sys/kernel/random/boot_id`), and on Linux the [`CONTAINER_MARKERS`]; each id
    /// replaced by its development seam where one is set. Windows reads neither id yet (stage 8),
    /// so a Windows node is its own machine.
    Host,
    /// Fixed answers that read nothing: every test's arm and each simulated node's.
    Fixed {
        /// The platform id, or `None` for a node whose platform id cannot be read.
        platform: Option<String>,
        /// The boot id, or `None` for a node whose boot cannot be read.
        boot: Option<String>,
        /// Whether the node runs in a container.
        container: bool,
    },
}

impl PlatformId {
    /// The machine's platform id, or `None` where it cannot be read.
    #[must_use]
    pub fn platform_id(&self) -> Option<String> {
        match self {
            Self::Host => seam_or(crate::dev_seam!(PLATFORM_ID_SEAM), host_platform_id),
            Self::Fixed { platform, .. } => platform.clone(),
        }
    }

    /// This boot's id, or `None` where it cannot be read.
    #[must_use]
    pub fn boot_id(&self) -> Option<String> {
        match self {
            Self::Host => seam_or(crate::dev_seam!(BOOT_ID_SEAM), host_boot_id),
            Self::Fixed { boot, .. } => boot.clone(),
        }
    }

    /// Whether this node runs inside a container: on Linux, one of the [`CONTAINER_MARKERS`]
    /// exists; never elsewhere.
    #[must_use]
    pub fn in_container(&self) -> bool {
        match self {
            Self::Host => cfg!(target_os = "linux") && in_container_by(marker_exists),
            Self::Fixed { container, .. } => *container,
        }
    }
}

/// A seam's answer where it is set (its trimmed text, or `None` when blank or not UTF-8), else
/// the host's.
fn seam_or(seam: Option<OsString>, host: impl FnOnce() -> Option<String>) -> Option<String> {
    match seam {
        Some(value) => value
            .to_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        None => host(),
    }
}

/// Whether any of the [`CONTAINER_MARKERS`] exists, by `exists`.
#[must_use]
pub fn in_container_by(exists: impl Fn(&str) -> bool) -> bool {
    CONTAINER_MARKERS.iter().any(|marker| exists(marker))
}

/// Whether a node is ephemeral (§6.3): it spends no number, publishes no anchor and stays `m0`
/// unless a person names it. Its model word is [`VM_WORD`](super::VM_WORD) (a hypervisor identity
/// on any OS), it runs in a container, or its aterm.toml says `[identity] ephemeral = true`
/// (`configured`). VM templates share platform ids, and containers on one host share its boot id
/// and often an image-baked machine id, so either would otherwise group as one machine.
#[must_use]
pub fn is_ephemeral(model_word: Option<&str>, in_container: bool, configured: bool) -> bool {
    configured || in_container || model_word == Some(super::VM_WORD)
}

#[cfg(not(target_family = "wasm"))]
fn host_platform_id() -> Option<String> {
    aterm_uds::process::host_id()
}

#[cfg(target_family = "wasm")]
fn host_platform_id() -> Option<String> {
    None
}

#[cfg(not(target_family = "wasm"))]
fn host_boot_id() -> Option<String> {
    aterm_uds::process::boot_id()
}

#[cfg(target_family = "wasm")]
fn host_boot_id() -> Option<String> {
    None
}

/// Whether `path` names an entry, a dangling link included, without following it.
fn marker_exists(path: &str) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
