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
mod tests {
    use super::*;

    #[test]
    fn the_registration_is_limas_pointed_at_the_vz_share() {
        // Lima 2.2.1's own, read from its `limactl` (the design's fact 22).
        let lima = r":rosetta:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/mnt/lima-rosetta/rosetta:OCF";
        assert_eq!(rosetta_registration("/mnt/lima-rosetta/rosetta"), lima);
        let vz = rosetta_registration(&format!("{ROSETTA_SHARE}/rosetta"));
        assert!(vz.ends_with(":/mnt/aterm-rosetta/rosetta:OCF"), "{vz}");
        assert!(!vz.contains('\n'));
    }

    #[test]
    fn the_unit_starts_early_wants_the_network_and_runs_from_the_boot_share() {
        let lines: Vec<&str> = UNIT.lines().collect();
        for want in [
            "After=local-fs.target",
            "Wants=network-online.target",
            "ExecStart=/mnt/aterm-boot/aterm-vz-agent",
            "Restart=no",
            "StandardOutput=journal+console",
        ] {
            assert!(lines.contains(&want), "{want}");
        }
        assert!(
            !lines.iter().any(|l| l.starts_with("After=network")),
            "the agent starts before the network, so the host watches the rest of the boot"
        );
    }

    #[test]
    fn a_changed_unit_is_rewritten_and_a_missing_or_equal_one_is_left() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let unit = dir.path().join("aterm-vz-agent.service");
        assert!(!refresh_unit(&unit, UNIT).unwrap());
        assert!(!unit.exists(), "a missing unit is the bake's to write");
        std::fs::write(&unit, UNIT).unwrap();
        assert!(!refresh_unit(&unit, UNIT).unwrap());
        std::fs::write(&unit, "[Service]\nExecStart=/old\n").unwrap();
        assert!(refresh_unit(&unit, UNIT).unwrap());
        assert_eq!(std::fs::read_to_string(&unit).unwrap(), UNIT);
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["aterm-vz-agent.service"], "no partial is left");

        #[cfg(unix)]
        {
            let decoy = dir.path().join("decoy");
            std::fs::write(&decoy, "untouched").unwrap();
            std::os::unix::fs::symlink(&decoy, dir.path().join(".aterm-vz-agent.service.partial"))
                .unwrap();
            std::fs::write(&unit, "[Service]\nExecStart=/stale\n").unwrap();
            assert!(refresh_unit(&unit, UNIT).unwrap());
            assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "untouched");

            let unit_link = dir.path().join("linked.service");
            std::os::unix::fs::symlink(&decoy, &unit_link).unwrap();
            assert!(refresh_unit(&unit_link, UNIT).is_err());
            assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "untouched");

            assert!(refresh_unit(Path::new("/dev/null"), UNIT).is_err());
        }
        let oversized = dir.path().join("oversized.service");
        std::fs::write(&oversized, vec![b'x'; MAX_UNIT_BYTES + 1]).unwrap();
        assert!(refresh_unit(&oversized, UNIT).is_err());
    }
}
