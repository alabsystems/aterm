// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn assertion(kind: &str, name: &str) -> Assertion {
    Assertion {
        kind: kind.to_string(),
        name: Some(name.to_string()),
    }
}

/// What the power manager lists for the mug: both of its activity's assertions, and one of
/// another holder's.
fn mugs() -> Vec<Assertion> {
    vec![
        assertion("PreventUserIdleDisplaySleep", AWAKE_REASON),
        assertion(SYSTEM_ASSERTION, AWAKE_REASON),
        assertion(SYSTEM_ASSERTION, "com.apple.audio.context"),
    ]
}

#[test]
fn the_assertions_are_read_by_reason() {
    let mut kinds = assertion_types(&mugs(), AWAKE_REASON);
    kinds.sort();
    assert_eq!(
        kinds,
        ["PreventUserIdleDisplaySleep", "PreventUserIdleSystemSleep"]
    );
    assert!(
        assertion_types(&mugs(), "aterm aCaffeine").is_empty(),
        "a prefix is not it"
    );
    let unnamed = [Assertion {
        kind: SYSTEM_ASSERTION.to_string(),
        name: None,
    }];
    assert!(assertion_types(&unnamed, AWAKE_REASON).is_empty());
}

/// A running mug is held only while the power manager lists its system assertion under the
/// mug's own pid; the trouble it recorded is what a mug that is not held, or not running,
/// says.
#[test]
fn held_means_macos_lists_the_mugs_assertion() {
    fn judge(
        on: bool,
        mug: Option<u32>,
        trouble: Option<String>,
        assertions_of: impl FnOnce(u32) -> Result<Vec<Assertion>, String>,
    ) -> Held {
        super::judge(on, false, Machine::default(), mug, trouble, assertions_of)
    }
    let listed = |pid: u32| {
        assert_eq!(pid, 4242, "read for the mug's pid");
        Ok(mugs())
    };
    assert_eq!(judge(false, Some(4242), None, listed), Held::Off);
    assert_eq!(
        judge(true, Some(4242), None, listed),
        Held::Held { pid: 4242 }
    );
    assert_eq!(
        judge(true, Some(7), Some("refused".to_string()), |_| Ok(
            Vec::new()
        )),
        Held::NotHeld {
            pid: 7,
            why: Some("refused".to_string())
        }
    );
    let display_only = |_| Ok(vec![assertion("PreventUserIdleDisplaySleep", AWAKE_REASON)]);
    assert_eq!(
        judge(true, Some(4242), None, display_only),
        Held::NotHeld {
            pid: 4242,
            why: None
        }
    );
    assert_eq!(
        judge(true, None, Some("disk full".to_string()), |_| panic!(
            "no mug, nothing to ask the power manager"
        )),
        Held::Stopped {
            why: Some("disk full".to_string())
        }
    );
    assert_eq!(
        judge(true, Some(1), None, |_| Err("no answer".to_string())),
        Held::Unread {
            pid: 1,
            why: "no answer".to_string()
        }
    );
}

/// With the lid closed and Keep Awake with Lid Closed off, a mug that holds nothing is
/// paused, not failing; one macOS still lists an assertion of is paused and lingering. With
/// the option on, held means the system assertion is listed too, lid open or closed.
/// NEGATIVE CONTROL: the same mug, its activity alone listed, is held by default and not
/// held with the option on.
#[test]
fn a_closed_lid_expects_nothing_unless_the_person_opted_in() {
    let closed = Machine {
        lid_closed: true,
        ..Machine::default()
    };
    let none = |_| Ok(Vec::new());
    assert_eq!(
        judge(true, false, closed, Some(9), None, none),
        Held::Paused {
            pid: 9,
            why: Pause::LidClosed,
            lingering: false
        }
    );
    assert_eq!(
        judge(true, false, closed, Some(9), None, |_| Ok(mugs())),
        Held::Paused {
            pid: 9,
            why: Pause::LidClosed,
            lingering: true
        }
    );
    let dark = Machine {
        dark_wake: true,
        ..Machine::default()
    };
    assert_eq!(
        judge(true, false, dark, Some(9), None, none),
        Held::Paused {
            pid: 9,
            why: Pause::Asleep,
            lingering: false
        }
    );
    assert_eq!(
        judge(true, true, dark, Some(9), None, none),
        Held::Paused {
            pid: 9,
            why: Pause::Asleep,
            lingering: false
        },
        "a dark wake pauses the opt-in too"
    );
    assert_eq!(
        judge(false, false, closed, Some(9), None, |_| panic!(
            "off: not read"
        )),
        Held::Off
    );
    let both = |_| {
        let mut held = mugs();
        held.push(assertion(LID_ASSERTION, AWAKE_REASON));
        Ok(held)
    };
    for machine in [Machine::default(), closed] {
        assert_eq!(
            judge(true, true, machine, Some(9), None, both),
            Held::Held { pid: 9 },
            "{machine:?}"
        );
        assert_eq!(
            judge(true, true, machine, Some(9), None, |_| Ok(mugs())),
            Held::NotHeld { pid: 9, why: None },
            "{machine:?}: no system assertion"
        );
    }
    assert_eq!(
        judge(true, false, Machine::default(), Some(9), None, |_| Ok(
            mugs()
        )),
        Held::Held { pid: 9 },
        "CONTROL"
    );
}
