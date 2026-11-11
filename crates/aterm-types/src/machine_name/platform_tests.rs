// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{PlatformId, in_container_by, is_ephemeral, seam_or};

#[test]
fn the_fixed_arm_answers_what_it_holds() {
    let node = PlatformId::Fixed {
        platform: Some("made-up-platform-1".to_owned()),
        boot: Some("made-up-boot-1".to_owned()),
        container: true,
    };
    assert_eq!(node.platform_id().as_deref(), Some("made-up-platform-1"));
    assert_eq!(node.boot_id().as_deref(), Some("made-up-boot-1"));
    assert!(node.in_container());
    let blank = PlatformId::Fixed {
        platform: None,
        boot: None,
        container: false,
    };
    assert_eq!((blank.platform_id(), blank.boot_id()), (None, None));
    assert!(!blank.in_container());
}

#[test]
fn a_set_seam_replaces_the_host_read_and_a_blank_one_reads_as_none() {
    let unasked = || -> Option<String> { panic!("the host read with a seam set") };
    assert_eq!(
        seam_or(Some(" lab-a ".into()), unasked).as_deref(),
        Some("lab-a")
    );
    assert_eq!(seam_or(Some("  ".into()), unasked), None);
    assert_eq!(
        seam_or(None, || Some("host".to_owned())).as_deref(),
        Some("host")
    );
}

#[test]
fn any_container_marker_makes_a_container() {
    for marker in super::CONTAINER_MARKERS {
        assert!(in_container_by(|path| path == marker), "{marker}");
    }
    assert!(!in_container_by(|_| false));
    assert!(
        !in_container_by(|path| path == "/run/systemd/system"),
        "systemd running on the host is no container"
    );
}

#[test]
fn a_vm_a_container_or_the_setting_makes_a_node_ephemeral() {
    assert!(is_ephemeral(Some("vm"), false, false));
    assert!(is_ephemeral(Some("mac-mini"), true, false));
    assert!(is_ephemeral(Some("mac-mini"), false, true));
    assert!(is_ephemeral(None, false, true));
    assert!(!is_ephemeral(Some("mac-mini"), false, false));
    assert!(!is_ephemeral(None, false, false));
    assert!(
        !is_ephemeral(Some("vmware-workstation"), false, false),
        "only the whole word vm"
    );
}
