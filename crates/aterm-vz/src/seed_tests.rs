// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        "  - [systemctl, stop, apt-daily.timer, apt-daily-upgrade.timer, \
         unattended-upgrades.service, ssh.socket]\n  - [systemctl, mask, apt-daily.timer, \
         apt-daily-upgrade.timer, unattended-upgrades.service, ssh.socket]\n",
        "  - [apt-mark, hold, linux-image-6.8.0-142-generic, linux-modules-6.8.0-142-generic]\n",
        "  - [dpkg-divert, --local, --rename, --divert, \
         /etc/initramfs/post-update.d/flash-kernel.aterm-vz, --add, \
         /etc/initramfs/post-update.d/flash-kernel]\n",
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
