// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this Mac is kept awake now, as macOS says it ([`held`]): the mug holding the lock, and
//! the assertions the power manager lists for its pid under [`AWAKE_REASON`]
//! ([`aterm_sysprobe::power::assertions_of`], in-process, the rows `pmset -g assertions` prints),
//! judged against what the mug holds for the lid and the option as they read now
//! ([`crate::model::hold`]). `aterm doctor`'s `awake:` row reads it; `status` says only what was
//! saved.

use aterm_sysprobe::power::Assertion;

use crate::model::{Hold, Machine, Pause};

/// What the power manager names the activity: the mug's, and a bridge's across an update
/// restart.
pub const AWAKE_REASON: &str = "aterm aCaffeine is keeping this Mac awake";

/// The assertion types in `held` named `reason` (`PreventUserIdleSystemSleep`,
/// `PreventUserIdleDisplaySleep`).
#[must_use]
pub fn assertion_types(held: &[Assertion], reason: &str) -> Vec<String> {
    held.iter()
        .filter(|assertion| assertion.name.as_deref() == Some(reason))
        .map(|assertion| assertion.kind.clone())
        .collect()
}

/// The assertion that keeps the system from idle sleep, which the mug's activity holds.
pub const SYSTEM_ASSERTION: &str = "PreventUserIdleSystemSleep";

/// The assertion Keep Awake with Lid Closed holds besides the activity
/// ([`aterm_sysprobe::power::PREVENT_SYSTEM_SLEEP`]).
pub const LID_ASSERTION: &str = aterm_sysprobe::power::PREVENT_SYSTEM_SLEEP;

/// The machine as it reads now for the mug's hold ([`crate::model::hold`]): the lid
/// ([`aterm_sysprobe::power::lid_closed`]) and whether this is a dark wake
/// ([`aterm_sysprobe::power::wake`]), both registry reads that need no window server. A
/// going-to-sleep is the mug's own to hear, so it reads as none here.
#[must_use]
pub fn machine_now() -> Machine {
    Machine {
        lid_closed: aterm_sysprobe::power::lid_closed() == Some(true),
        dark_wake: aterm_sysprobe::power::wake() == Some(aterm_sysprobe::power::Wake::Dark),
        going_to_sleep: false,
    }
}

/// Whether this Mac is kept awake now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// This process is not the login's, or not on macOS: why. Nothing was read.
    NotHere(String),
    /// The person turned keep-awake off.
    Off,
    /// The mug (`pid`) runs, and macOS lists every assertion it holds now: the activity's, and
    /// with Keep Awake with Lid Closed on the system assertion too.
    Held {
        /// The mug's process.
        pid: u32,
    },
    /// The mug (`pid`) runs with the mode on, and holds nothing now, by design: `why` (the lid
    /// is closed and Keep Awake with Lid Closed is off, or this is a dark wake, which pauses it
    /// whatever the option). It holds again
    /// once the lid opens, or the Mac wakes fully.
    Paused {
        /// The mug's process.
        pid: u32,
        /// Why it holds nothing.
        why: Pause,
        /// macOS still lists an assertion of the mug's: it lets go at its next look.
        lingering: bool,
    },
    /// The mug (`pid`) runs, but macOS lists no assertion of it, or not the system assertion Keep
    /// Awake with Lid Closed holds; `why` is the recorded trouble.
    NotHeld {
        /// The mug's process.
        pid: u32,
        /// What keeps it from holding, when the mug recorded it ([`crate::trouble`]).
        why: Option<String>,
    },
    /// No mug runs; `why` is the recorded trouble.
    Stopped {
        /// What keeps it from running, when it was recorded ([`crate::trouble`]).
        why: Option<String>,
    },
    /// The mug (`pid`) runs, and the power manager did not answer: why.
    Unread {
        /// The mug's process.
        pid: u32,
        /// Why the power manager gave no answer.
        why: String,
    },
}

/// [`Held`] from what was read: the saved mode's being on, Keep Awake with Lid Closed
/// (`lid_awake`), the `machine` as it reads now, the running mug's pid, the trouble recorded,
/// and the assertions the power manager lists for a pid (or why it gave none). What the mug
/// should hold is [`crate::model::hold`]'s answer, and each assertion that answer names must be
/// listed.
#[must_use]
pub fn judge(
    on: bool,
    lid_awake: bool,
    machine: Machine,
    mug: Option<u32>,
    trouble: Option<String>,
    assertions_of: impl FnOnce(u32) -> Result<Vec<Assertion>, String>,
) -> Held {
    let hold = crate::model::hold(on, lid_awake, machine);
    if hold == Hold::Off {
        return Held::Off;
    }
    let Some(pid) = mug else {
        return Held::Stopped { why: trouble };
    };
    let listed = match assertions_of(pid) {
        Ok(held) => assertion_types(&held, AWAKE_REASON),
        Err(why) => return Held::Unread { pid, why },
    };
    let lists = |kind: &str| listed.iter().any(|listed| listed == kind);
    match hold {
        Hold::Paused(why) => Held::Paused {
            pid,
            why,
            lingering: !listed.is_empty(),
        },
        _ if lists(SYSTEM_ASSERTION) && (!hold.system() || lists(LID_ASSERTION)) => {
            Held::Held { pid }
        }
        _ => Held::NotHeld { pid, why: trouble },
    }
}

/// Whether this Mac is kept awake now, read off the login's files, the lock and the power
/// manager.
#[cfg(target_os = "macos")]
#[must_use]
pub fn held() -> Held {
    let paths = match crate::cli::login_paths() {
        Ok(paths) => paths,
        Err(why) => return Held::NotHere(why),
    };
    let Some(now) = crate::state::clocks() else {
        return Held::NotHere("the clocks cannot be read".to_string());
    };
    let on = crate::state::load(&paths.state_file(), now).is_on(now);
    let lid_awake = crate::state::load_lid(&paths.lid_file());
    let mug = crate::instance::running_mug(&paths.lock_file(), &paths.code_dir())
        .map(|holder| holder.pid);
    let trouble = crate::trouble::read(&paths).map(|trouble| trouble.why);
    judge(
        on,
        lid_awake,
        machine_now(),
        mug,
        trouble,
        aterm_sysprobe::power::assertions_of,
    )
}

/// Off macOS nothing keeps a Mac awake.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn held() -> Held {
    Held::NotHere("aCaffeine is macOS only".to_string())
}

#[cfg(test)]
mod tests {
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
}
