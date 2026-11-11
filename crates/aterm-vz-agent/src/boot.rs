// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the agent sets up at its start in the guest: the systemd unit it runs under
//! (rewritten for the next boot when the text it carries changed) and Rosetta's binfmt
//! registration. The texts are platform-neutral, so the host's tests hold them.

use std::io::{self, Write as _};
use std::path::Path;

/// Where the bake writes the agent's unit.
pub const UNIT_PATH: &str = "/etc/systemd/system/aterm-vz-agent.service";

/// The agent's unit: started after the local file systems (so the boot and Rosetta
/// shares are mounted) and before the network, which it `Wants=` into the boot
/// transaction so its readiness wait covers DHCP. It runs the agent the booting checkout
/// built, from the read-only boot share, and names no version and no user.
pub const UNIT: &str = "\
[Unit]
Description=aterm's guest agent (the Linux lane's commands over vsock)
After=local-fs.target
Wants=network-online.target

[Service]
ExecStart=/mnt/aterm-boot/aterm-vz-agent
Restart=no
StandardOutput=journal+console

[Install]
WantedBy=multi-user.target
";

/// Maximum size accepted when reading an existing guest systemd unit on disk.
const MAX_UNIT_BYTES: usize = 64 * 1024;

/// Rewrite the unit at `path` to `carried` when it holds other text, for the next boot:
/// `Ok(true)` when it did. A missing unit is the bake's to write, and stays missing.
///
/// # Errors
/// The unit could not be read, or the new text written and renamed over it.
pub fn refresh_unit(path: &Path, carried: &str) -> io::Result<bool> {
    let on_disk = match crate::read_bounded_regular(path, MAX_UNIT_BYTES) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if on_disk == carried.as_bytes() {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    if !std::fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unit parent is not a real directory",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let partial = path.with_file_name(format!(".{}.partial", name.to_string_lossy()));
    let _ = std::fs::remove_file(&partial);
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o644).custom_flags(crate::NONBLOCK_OPEN_FLAG);
        }
        let mut f = opts.open(&partial)?;
        f.write_all(carried.as_bytes())?;
        // A power loss may leave the old unit or the new one, never a torn one: the text
        // is on disk before the rename names it.
        #[expect(
            clippy::disallowed_methods,
            reason = "std-only for the musl guest build: aterm-durable is not in its graph; the whole unit is flushed before its rename"
        )]
        f.sync_all()?;
    }
    std::fs::rename(&partial, path)?;
    #[cfg(unix)]
    if let Ok(dir_file) = crate::open_nonblock(parent) {
        #[expect(
            clippy::disallowed_methods,
            reason = "std-only for the musl guest build: aterm-durable is not in its graph; the parent directory is synced after the rename"
        )]
        let _ = dir_file.sync_all();
    }
    Ok(true)
}

/// The kernel's binfmt_misc registration file.
pub const BINFMT_REGISTER: &str = "/proc/sys/fs/binfmt_misc/register";

/// The entry the registration makes, which the lane's Rosetta check reads.
pub const ROSETTA_ENTRY: &str = "/proc/sys/fs/binfmt_misc/rosetta";

/// Where the `rosetta` share (`VZLinuxRosettaDirectoryShare`) is mounted, by its fstab line.
pub const ROSETTA_SHARE: &str = "/mnt/aterm-rosetta";

/// Lima 2.2.1's registration (its boot script's `rosetta_binfmt`), pointed at
/// `interpreter`: x86_64 ELF executables, run by Rosetta with flags `OCF` (credentials,
/// the interpreter opened at registration, the binary's descriptor). The `\x` escapes
/// are binfmt_misc's own and are written as text.
#[must_use]
pub fn rosetta_registration(interpreter: &str) -> String {
    format!(
        r":rosetta:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:{interpreter}:OCF"
    )
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
