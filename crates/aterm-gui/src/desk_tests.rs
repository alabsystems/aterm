// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::Desk;

#[test]
fn without_the_seam_every_gate_is_todays() {
    for headless in [false, true] {
        let desk = Desk::new(headless, false);
        assert_ne!(desk, Desk::LabHeadless);
        assert_eq!(desk.kept(), !headless);
        for flag in [false, true] {
            assert_eq!(desk.on_glass(flag), flag);
            assert_eq!(desk.quit_keeps_agents(flag), flag);
        }
    }
}

#[test]
fn the_seam_reaches_a_headless_instance_only() {
    assert_eq!(Desk::new(false, true), Desk::Window);
    let lab = Desk::new(true, true);
    assert_eq!(lab, Desk::LabHeadless);
    assert!(lab.kept());
    assert!(lab.on_glass(false));
    assert!(lab.quit_keeps_agents(false));
}
