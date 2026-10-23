// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bake's NoCloud seed (`docs/DESIGN-vz-guest-2026-10-23.md` §4, §6, §8).
//!
//! Three files, `user-data`, `meta-data` and `network-config`, on an ISO labelled `cidata`
//! that `hdiutil makehybrid` makes (KEEP-OS, init-system row 1; fact 38). The seed is
//! attached read-only to a bake's one boot and never again: cloud-init sets up the lane
//! user, DHCP on `enp0s1`, the masks, the kernel holds, the two shares' fstab lines, and
//! the agent's and rosettad's units, which `runcmd` starts without waiting on them.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;

use crate::pins::KERNEL_RELEASE;

/// The seed ISO's volume label, which cloud-init's NoCloud source looks for.
pub const VOLUME: &str = "cidata";

/// The interface the guest's one NAT device appears as.
pub const INTERFACE: &str = "enp0s1";

/// Where the read-only boot share (the content-addressed agent, §6) is mounted.
pub const BOOT_MOUNT: &str = "/mnt/aterm-boot";

/// Where `VZLinuxRosettaDirectoryShare`'s share is mounted.
pub const ROSETTA_MOUNT: &str = "/mnt/aterm-rosetta";

/// The virtiofs tag of the boot share.
pub const BOOT_TAG: &str = "aterm-boot";

/// The virtiofs tag of the Rosetta share.
pub const ROSETTA_TAG: &str = "rosetta";

/// The agent's unit, `/etc/systemd/system/aterm-vz-agent.service` (§6): it runs the agent
/// from the boot share, after the local file systems (the boot share among them) and with
/// network-online's job in the boot transaction. V1's agent carries the same text and
/// rewrites the file for the next boot when the two differ.
pub const AGENT_UNIT: &str = "\
[Unit]
Description=aterm's guest agent (aterm-vz-agent)
After=local-fs.target
Wants=network-online.target

[Service]
ExecStart=/mnt/aterm-boot/aterm-vz-agent
Restart=no
StandardOutput=journal+console

[Install]
WantedBy=multi-user.target
";

/// `/etc/systemd/system/rosettad.service` in lima's shape (fact 22), pointed at the
/// Rosetta share: the lane's cache reset stops and starts it by name (fact 11), and its
/// socket is made reachable to unprivileged x86_64 processes at the path
/// `VZLinuxRosettaUnixSocketCachingOptions` names.
pub const ROSETTAD_UNIT: &str = "\
[Unit]
Description=Rosetta's AOT caching daemon for x86_64 programs (aterm-vz)
RequiresMountsFor=/mnt/aterm-rosetta

[Service]
RuntimeDirectory=rosettad
CacheDirectory=rosettad
ExecStartPre=/bin/rm -f /var/cache/rosettad/uds/rosetta.sock
ExecStart=/mnt/aterm-rosetta/rosettad daemon /var/cache/rosettad
ExecStartPost=/bin/sh -c 'while [ ! -S /var/cache/rosettad/uds/rosetta.sock ]; do sleep 0.1; done; chmod go+w /var/cache/rosettad/uds/rosetta.sock; ln -sf /var/cache/rosettad/uds/rosetta.sock /run/rosettad/rosetta.sock'
OOMPolicy=continue
OOMScoreAdjust=-500

[Install]
WantedBy=default.target
";

/// The units the bake masks: nothing upgrades the guest behind the lane, and nothing
/// listens (no ssh, §8).
pub const MASKED: [&str; 4] = [
    "apt-daily.timer",
    "apt-daily-upgrade.timer",
    "unattended-upgrades.service",
    "ssh.socket",
];

/// The lane user: the host's name and uid (lima's convention, fact 20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The login name, which the seed writes into YAML and into argv.
    pub name: String,
    /// The uid, the host's.
    pub uid: u32,
}

impl User {
    /// This process's user, by `getpwuid_r`.
    #[must_use]
    pub fn host() -> Option<Self> {
        let uid = aterm_uds::peer::our_uid();
        aterm_uds::account::user_name(uid).map(|name| Self { name, uid })
    }

    /// The guest home, `/home/<name>.guest` (lima's convention).
    #[must_use]
    pub fn home(&self) -> String {
        format!("/home/{}.guest", self.name)
    }
}

/// The seed's three files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    user_data: String,
    meta_data: String,
    network_config: String,
}

impl Seed {
    /// The seed for `user`.
    ///
    /// # Errors
    /// The name is not one Ubuntu's `useradd` takes by default (`[a-z_][a-z0-9_-]*`, at
    /// most 32), so it could not be written into the YAML and argv safely, or the uid is
    /// root's.
    pub fn new(user: &User) -> Result<Self, String> {
        let name = &user.name;
        let fits = !name.is_empty()
            && name.len() <= 32
            && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !fits {
            return Err(format!(
                "the lane user is the host's, and `{name}` is no name Ubuntu's useradd takes \
                 ([a-z_][a-z0-9_-]*, at most 32)"
            ));
        }
        if user.uid == 0 {
            return Err("the lane user is the host's, and the host user is root".into());
        }
        Ok(Self {
            user_data: user_data(user),
            meta_data: "instance-id: aterm-linux\nlocal-hostname: aterm-linux\n".into(),
            network_config: format!("version: 2\nethernets:\n  {INTERFACE}:\n    dhcp4: true\n"),
        })
    }

    /// The files, by the names the ISO holds them under, in a fixed order.
    #[must_use]
    pub fn files(&self) -> [(&'static str, &str); 3] {
        [
            ("user-data", &self.user_data),
            ("meta-data", &self.meta_data),
            ("network-config", &self.network_config),
        ]
    }

    /// FNV-1a/64 of the three files, names and bytes, as 16 hex digits: a baked disk's
    /// name carries it, so a changed seed bakes a new disk.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut h = aterm_hash::FNV64_OFFSET;
        for (name, text) in self.files() {
            h = aterm_hash::fnv1a_64_extend(h, name.as_bytes());
            h = aterm_hash::fnv1a_64_extend(h, b"\x1f");
            h = aterm_hash::fnv1a_64_extend(h, text.as_bytes());
            h = aterm_hash::fnv1a_64_extend(h, b"\x1e");
        }
        format!("{h:016x}")
    }

    /// Write the three files into the empty directory `dir`.
    ///
    /// # Errors
    /// A file could not be written.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        for (name, text) in self.files() {
            std::fs::write(dir.join(name), text)?;
        }
        Ok(())
    }
}

/// `text` indented by `pad` spaces on every line, for a YAML block scalar.
fn indented(text: &str, pad: usize) -> String {
    let pad = " ".repeat(pad);
    text.lines().fold(String::new(), |mut out, line| {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "{pad}{line}");
        }
        out
    })
}

/// The `#cloud-config` for `user` (its name already checked by [`Seed::new`]).
fn user_data(user: &User) -> String {
    let fstab = format!(
        "{BOOT_TAG} {BOOT_MOUNT} virtiofs ro 0 0\n{ROSETTA_TAG} {ROSETTA_MOUNT} virtiofs ro 0 0\n"
    );
    let mut out = String::from(
        "#cloud-config\n\
         # aterm-vz's bake seed (docs/DESIGN-vz-guest-2026-10-23.md §4): attached to one boot.\n",
    );
    let _ = write!(
        out,
        "users:\n  - name: {name}\n    uid: {uid}\n    homedir: {home}\n    shell: /bin/bash\n    \
         sudo: \"ALL=(ALL) NOPASSWD:ALL\"\n    lock_passwd: true\n\
         ssh_pwauth: false\npackage_update: false\npackage_upgrade: false\n",
        name = user.name,
        uid = user.uid,
        home = user.home(),
    );
    out.push_str("write_files:\n");
    for (path, text, append) in [
        (
            "/etc/systemd/system/aterm-vz-agent.service",
            AGENT_UNIT,
            false,
        ),
        ("/etc/systemd/system/rosettad.service", ROSETTAD_UNIT, false),
        ("/etc/fstab", fstab.as_str(), true),
    ] {
        let _ = write!(out, "  - path: {path}\n    permissions: \"0644\"\n");
        if append {
            out.push_str("    append: true\n");
        }
        let _ = write!(out, "    content: |\n{}", indented(text, 6));
    }
    out.push_str("runcmd:\n");
    let holds = format!("linux-image-{KERNEL_RELEASE}, linux-modules-{KERNEL_RELEASE}");
    for line in [
        format!("[mkdir, -p, {BOOT_MOUNT}, {ROSETTA_MOUNT}]"),
        format!("[mount, {BOOT_MOUNT}]"),
        format!("[mount, {ROSETTA_MOUNT}]"),
        format!("[systemctl, mask, --now, {}]", MASKED.join(", ")),
        format!("[apt-mark, hold, {holds}]"),
        format!("[loginctl, enable-linger, {}]", user.name),
        "[systemctl, daemon-reload]".to_string(),
        "[systemctl, enable, --now, --no-block, aterm-vz-agent.service, rosettad.service]"
            .to_string(),
    ] {
        let _ = writeln!(out, "  - {line}");
    }
    out
}

/// `hdiutil`'s arguments for the NoCloud ISO of the directory `files` at `iso` (fact 38).
#[must_use]
pub fn makehybrid_args(files: &Path, iso: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "makehybrid",
        "-iso",
        "-joliet",
        "-default-volume-name",
        VOLUME,
        "-o",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(iso.as_os_str().to_owned());
    args.push(files.as_os_str().to_owned());
    args
}

/// Make the seed ISO of `files` at `iso` with the OS's `hdiutil`, under a one-minute hang
/// detector (fact 38 measured 0.28 s).
///
/// # Errors
/// It could not start, did not finish, or exited unsuccessfully (with its stderr).
pub fn make_iso(files: &Path, iso: &Path) -> Result<(), String> {
    let hdiutil = aterm_uds::os_path!("hdiutil");
    let mut cmd = aterm_uds::spawnfd::command(hdiutil);
    cmd.args(makehybrid_args(files, iso));
    let out = aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
        .map_err(|e| format!("{hdiutil} makehybrid {e}"))?;
    if out.status.success() && iso.is_file() {
        Ok(())
    } else {
        Err(format!(
            "{hdiutil} makehybrid exited {:?} and made no {}: {}",
            out.status.code(),
            iso.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> User {
        User {
            name: "lane".into(),
            uid: 501,
        }
    }

    #[test]
    fn the_seeds_three_files_set_up_the_lane_user_the_network_and_the_units() {
        let seed = Seed::new(&lane()).unwrap();
        let [(user_name, user), (meta_name, meta), (net_name, net)] = seed.files();
        assert_eq!(
            [user_name, meta_name, net_name],
            ["user-data", "meta-data", "network-config"]
        );
        assert!(user.starts_with("#cloud-config\n"), "{user}");
        for needle in [
            "  - name: lane\n    uid: 501\n    homedir: /home/lane.guest\n    shell: /bin/bash\n",
            "    sudo: \"ALL=(ALL) NOPASSWD:ALL\"\n",
            "  - path: /etc/systemd/system/aterm-vz-agent.service\n",
            "      ExecStart=/mnt/aterm-boot/aterm-vz-agent\n",
            "      After=local-fs.target\n      Wants=network-online.target\n",
            "      ExecStart=/mnt/aterm-rosetta/rosettad daemon /var/cache/rosettad\n",
            "      RequiresMountsFor=/mnt/aterm-rosetta\n",
            "  - path: /etc/fstab\n    permissions: \"0644\"\n    append: true\n",
            "      aterm-boot /mnt/aterm-boot virtiofs ro 0 0\n      rosetta /mnt/aterm-rosetta virtiofs ro 0 0\n",
            "  - [systemctl, mask, --now, apt-daily.timer, apt-daily-upgrade.timer, \
             unattended-upgrades.service, ssh.socket]\n",
            "  - [apt-mark, hold, linux-image-6.8.0-142-generic, linux-modules-6.8.0-142-generic]\n",
            "  - [loginctl, enable-linger, lane]\n",
            "  - [systemctl, enable, --now, --no-block, aterm-vz-agent.service, rosettad.service]\n",
        ] {
            assert!(user.contains(needle), "{needle}\n---\n{user}");
        }
        let mount = user.find("[mount, /mnt/aterm-boot]").unwrap();
        let start = user.find("[systemctl, enable, --now").unwrap();
        assert!(mount < start, "the agent starts once its share is mounted");
        assert!(
            !user.contains("- default"),
            "no stock ubuntu user beside the lane's"
        );
        assert_eq!(
            meta,
            "instance-id: aterm-linux\nlocal-hostname: aterm-linux\n"
        );
        assert_eq!(
            net, "version: 2\nethernets:\n  enp0s1:\n    dhcp4: true\n",
            "DHCP on the NAT device removes the two-minute wait (fact 38)"
        );
    }

    #[test]
    fn the_fingerprint_follows_every_byte_of_the_seed() {
        let seed = Seed::new(&lane()).unwrap();
        assert_eq!(seed.fingerprint(), seed.clone().fingerprint());
        assert!(crate::is_hex(&seed.fingerprint(), 16));
        let other = Seed::new(&User { uid: 502, ..lane() }).unwrap();
        assert_ne!(
            seed.fingerprint(),
            other.fingerprint(),
            "the uid is in the seed"
        );
    }

    #[test]
    fn a_name_the_seed_cannot_carry_is_refused() {
        for name in [
            "",
            "Andrew",
            "a b",
            "x:y",
            "ay\nroot",
            "-x",
            &"a".repeat(33),
        ] {
            assert!(
                Seed::new(&User {
                    name: name.into(),
                    uid: 501
                })
                .is_err(),
                "{name:?}"
            );
        }
        assert!(Seed::new(&User { uid: 0, ..lane() }).is_err());
        assert!(
            Seed::new(&User {
                name: "_x-1".into(),
                uid: 501
            })
            .is_ok()
        );
    }

    #[test]
    fn the_iso_is_made_by_hdiutils_makehybrid_with_the_cidata_label() {
        let args = makehybrid_args(Path::new("/s/files"), Path::new("/s/seed.iso"));
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "makehybrid",
                "-iso",
                "-joliet",
                "-default-volume-name",
                "cidata",
                "-o",
                "/s/seed.iso",
                "/s/files"
            ]
        );
        assert_eq!(aterm_uds::os_path!("hdiutil"), "/usr/bin/hdiutil");
    }

    #[test]
    fn the_seed_writes_its_three_files() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let seed = Seed::new(&lane()).unwrap();
        seed.write_to(dir.path()).unwrap();
        for (name, text) in seed.files() {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(name)).unwrap(),
                text
            );
        }
    }
}
