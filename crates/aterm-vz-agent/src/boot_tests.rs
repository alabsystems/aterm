// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
