// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn plan() -> Plan {
    Plan {
        name: "aterm-linux".into(),
        cpus: CPUS,
        memory_bytes: MEMORY,
        kernel: "/vz/kernel/k/Image".into(),
        initrd: "/vz/kernel/k/initrd".into(),
        command_line: COMMAND_LINE.into(),
        disks: vec![
            Disk {
                path: "/vz/live/aterm-linux.img".into(),
                read_only: false,
            },
            Disk {
                path: "/vz/seed/f.iso".into(),
                read_only: true,
            },
        ],
        mac: mac_for("aterm-linux"),
        boot_share: "/vz/agent/abc".into(),
        shares: vec![
            ShareEntry {
                name: "repo".into(),
                path: "/src/aterm".into(),
            },
            ShareEntry {
                name: "git".into(),
                path: "/src/aterm/.git".into(),
            },
        ],
        rosetta: true,
        console_log: "/vz/run/console.log".into(),
        agent_sha256: "ab".repeat(32),
    }
}

fn limits(max_cpus: u32, host_cpus: u32) -> Limits {
    Limits {
        max_cpus,
        max_memory: 128 << 30,
        host_cpus,
        host_memory: 128 << 30,
    }
}

#[test]
fn cpus_are_clamped_by_the_framework_and_the_host_and_never_zero() {
    assert_eq!(limits(64, 18).cpus(8), 8);
    assert_eq!(limits(64, 4).cpus(8), 4);
    assert_eq!(limits(2, 18).cpus(8), 2);
    assert_eq!(limits(0, 0).cpus(8), 1);
}

#[test]
fn memory_past_the_framework_or_the_hosts_margin_is_refused_with_the_figures() {
    assert_eq!(limits(64, 18).memory_refusal(MEMORY), None);
    let mut l = limits(64, 18);
    l.max_memory = 8 << 30;
    let why = l.memory_refusal(MEMORY).expect("over the framework's");
    assert!(
        why.starts_with("NOT RUN") && why.contains("16.0 GiB") && why.contains("8.0 GiB"),
        "{why}"
    );
    let mut l = limits(64, 18);
    l.host_memory = 16 << 30;
    let why = l.memory_refusal(MEMORY).expect("a small host");
    assert!(why.contains("need 24.0 GiB"), "{why}");
    l.host_memory = 24 << 30;
    assert_eq!(
        l.memory_refusal(MEMORY),
        None,
        "exactly the margin is enough"
    );
}

#[test]
fn the_mac_is_fixed_by_the_name_locally_administered_and_unicast() {
    let mac = mac_for("aterm-linux");
    assert_eq!(mac, mac_for("aterm-linux"));
    assert_ne!(mac, mac_for("aterm-linux-2"));
    assert_eq!(mac[0] & 0x01, 0, "unicast");
    assert_eq!(mac[0] & 0x02, 0x02, "locally administered");
    let text = mac_text(&mac);
    assert_eq!(text.len(), 17);
    assert_eq!(text.matches(':').count(), 5);
    assert_eq!(mac_text(&[0x02, 0, 0xab, 1, 2, 0xff]), "02:00:ab:01:02:ff");
}

#[test]
fn the_configuration_text_names_every_part_of_the_designs_table() {
    let text = plan().config_text(&limits(64, 4));
    for want in [
        "platform: VZGenericPlatformConfiguration",
        "cmdline=\"console=hvc0 root=/dev/vda rw systemd.show_status=error\"",
        "cpus: 4 (asked 8, framework max 64, host 4)",
        "memory: 16.0 GiB",
        "disk vda: VZVirtioBlockDeviceConfiguration /vz/live/aterm-linux.img writable",
        "disk vdb: VZVirtioBlockDeviceConfiguration /vz/seed/f.iso read-only",
        "on VZNATNetworkDeviceAttachment, mac ",
        "listener on port 1025",
        "share aterm-boot: VZSingleDirectoryShare /vz/agent/abc read-only",
        "share aterm: VZMultipleDirectoryShare [repo=/src/aterm git=/src/aterm/.git] read-only",
        "share rosetta: VZLinuxRosettaDirectoryShare, caching at /run/rosettad/rosetta.sock",
        "console: VZVirtioConsoleDeviceSerialPortConfiguration to /vz/run/console.log",
        "entropy: VZVirtioEntropyDeviceConfiguration",
        "absent: graphics, keyboard, pointing device, audio, balloon",
    ] {
        assert!(text.contains(want), "{want} in\n{text}");
    }
    let mut no_rosetta = plan();
    no_rosetta.rosetta = false;
    assert!(!no_rosetta.config_text(&limits(64, 18)).contains("Rosetta"));
}
