// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux lane's own VM, which retires lima (`docs/DESIGN-vz-guest-2026-10-23.md`).
//!
//! This library holds the image and the disks (the design's slice V3):
//! - [`pins`]: Ubuntu's three pinned release files, the gunzipped kernel's recorded sha256,
//!   and the words a Mac reads once Ubuntu archives the pinned release (§3);
//! - [`kernel`]: the in-house gunzip of the pinned `vmlinuz` to the arm64 `Image`, held to
//!   its magic and its sha256, laid beside the initrd (§3, §4);
//! - [`layers`]: the base, baked and live disks: APFS clones, the partial name and the seal,
//!   the live record, the free-space floor at every bring-up, and the reclaim (§4);
//! - [`seed`]: the bake's NoCloud seed, its three files and its ISO (§4).
//!
//! It loads no framework and starts no VM. The helper that does (`aterm-vz-host`, its
//! `Machine` port and protocol) is slice V2, the agent's client comes with V1's codec, and
//! the lab's `Driver` is V4. Where they join: [`layers::Prepared::Bake`] hands V4's bake
//! the disk it boots through V2's helper, with the ISO [`layers::Layers::seed_iso`] makes;
//! [`layers::Prepared::Live`] names the disk every later boot attaches; and
//! [`space::RUN_FLOOR`] is the floor V2's helper stops a running VM at, in
//! [`space::below_run`]'s words.
//!
//! The lane takes its inputs only from what `verify provision --linux` placed and stamped
//! for the pin ([`pins::base`], [`pins::kernel_files`]), and reading them at a bring-up marks
//! their rows used, so the tooling cache's idle reclaim keeps them while the lane runs.

pub mod clone;
pub mod kernel;
pub mod layers;
pub mod pins;
pub mod seed;
pub mod space;

/// Why the lane cannot have its disks or its kernel, in the words its report prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The volume is below a free-space floor ([`space`]): the lane is NOT RUN, and nothing
    /// was cloned or baked.
    Floor(String),
    /// A pinned input is missing or is not the pin's: COULD NOT RUN, naming what provides it.
    Input(String),
    /// A file operation failed: a write to a full volume is one, never a bad input.
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Floor(why) | Self::Input(why) | Self::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// `what` failed on `path` with `e`, as an [`Error::Io`].
pub(crate) fn io(what: &str, path: &std::path::Path, e: &std::io::Error) -> Error {
    Error::Io(format!("could not {what} {}: {e}", path.display()))
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Whether `text` is a lowercase-hex digest of `len` digits.
pub(crate) fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Make `path` read-only to everyone (0444), as a sealed disk and the kernel files are.
pub(crate) fn read_only(path: &std::path::Path) -> Result<(), Error> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(0o444)
    };
    #[cfg(not(unix))]
    let permissions = {
        let mut p = std::fs::metadata(path)
            .map_err(|e| io("read", path, &e))?
            .permissions();
        p.set_readonly(true);
        p
    };
    std::fs::set_permissions(path, permissions).map_err(|e| io("make read-only", path, &e))
}

/// Make the clone `path` writable by its owner (0644): a clone carries its source's 0444.
pub(crate) fn writable(path: &std::path::Path) -> Result<(), Error> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::Permissions::from_mode(0o644)
    };
    #[cfg(not(unix))]
    let permissions = {
        let mut p = std::fs::metadata(path)
            .map_err(|e| io("read", path, &e))?
            .permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "off Unix the read-only bit is the only permission a clone carries"
        )]
        p.set_readonly(false);
        p
    };
    std::fs::set_permissions(path, permissions).map_err(|e| io("make writable", path, &e))
}
