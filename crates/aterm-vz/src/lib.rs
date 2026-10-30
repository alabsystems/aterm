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
//! And the helper, which runs the VM (slice V2, §5 and §5.1):
//! - [`plan`]: what the helper builds, and the framework's and the host's limits (§5, §9);
//! - [`machine`]: the `Machine` port, its VZ arm in `aterm-vz-host` (macOS only, over
//!   `aterm-objc` and a `dlopen`ed Virtualization.framework) and its Fake arm;
//! - [`console`] and [`detect`]: the console watcher and the boot, heartbeat and free-space
//!   detectors (§10);
//! - [`proto`] and [`transport`]: the control protocol and its framing with descriptors;
//! - [`record`]: the run files, the record, the disk's `flock` and the escalation by pid and
//!   unique id;
//! - [`helper`]: the helper's core (sessions, `keep`, the release rule, `Release`) and its
//!   serve loop;
//! - [`client`]: the `Helper` port (the lab's side), its socket arm with the detached start
//!   and its Fake arm;
//! - [`sign`]: the content-addressed ad hoc signing of the helper the lab built;
//! - [`agent`]: the agent's command client over a vsock descriptor (`Exec`, `Poweroff`);
//! - [`key`]: the boot's key, written to the key disk only guest root reads, which the
//!   agent's `Hello` must carry before the helper believes any frame of its dial.
//!
//! The library loads no framework and starts no VM: only `aterm-vz-host` opens
//! Virtualization.framework, at `Boot`, so linking this library (the lab and its tests do)
//! loads nothing of it.
//!
//! The lane takes its inputs only from what `verify provision --linux` placed and stamped
//! for the pin ([`pins::base`], [`pins::kernel_files`]); reading them at a bring-up marks
//! their rows used, so the tooling cache's idle reclaim keeps them while the lane runs.
//!
//! The lifetime is a model (slice V5): `VzLifecycle` (`aterm_models_misc::vz_lifecycle_model`),
//! whose actions the core, the client, the run files and the layers carry as anchors in test
//! and debug builds, bound by `tests/conformance_lifecycle.rs`; `fault` holds its negative
//! controls' planted defects, and `lifecycle` the steps that are the world's or the lab's.

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

#[cfg(unix)]
pub mod agent;
#[cfg(unix)]
pub mod client;
pub mod clone;
pub mod console;
pub mod detect;
#[cfg(any(test, debug_assertions))]
pub mod fault;
#[cfg(unix)]
pub mod helper;
pub mod kernel;
#[cfg(unix)]
pub mod key;
pub mod layers;
#[cfg(any(test, debug_assertions))]
pub mod lifecycle;
#[cfg(unix)]
pub mod machine;
pub mod pins;
pub mod plan;
#[cfg(unix)]
pub mod proto;
#[cfg(unix)]
pub mod record;
pub mod seed;
#[cfg(unix)]
pub mod sign;
pub mod space;
#[cfg(unix)]
pub mod transport;

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

/// Read a small UTF-8 regular file without following a terminal symlink or blocking on a FIFO.
pub(crate) fn read_small_text(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    use std::io::Read as _;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let file = {
        if !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
        std::fs::File::open(path).ok()?
    };
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let cap = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(cap).read_to_end(&mut bytes).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
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
