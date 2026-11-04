// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this Mac is kept awake now, as macOS says it ([`held`]): the mug holding the lock, and
//! the assertion the power manager lists for its pid under [`AWAKE_REASON`]
//! ([`aterm_sysprobe::power::assertions_of`], in-process, the rows `pmset -g assertions` prints).
//! `aterm doctor`'s `awake:` row reads it; `status` says only what was saved.

use aterm_sysprobe::power::Assertion;

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

/// Whether this Mac is kept awake now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// This process is not the login's, or not on macOS: why. Nothing was read.
    NotHere(String),
    /// The person turned keep-awake off.
    Off,
    /// The mug (`pid`) runs, and macOS lists its assertion.
    Held {
        /// The mug's process.
        pid: u32,
    },
    /// The mug (`pid`) runs, but macOS lists no assertion of it; `why` is the recorded trouble.
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

/// [`Held`] from what was read: the saved mode's being on, the running mug's pid, the trouble
/// recorded, and the assertions the power manager lists for a pid (or why it gave none).
#[must_use]
pub fn judge(
    on: bool,
    mug: Option<u32>,
    trouble: Option<String>,
    assertions_of: impl FnOnce(u32) -> Result<Vec<Assertion>, String>,
) -> Held {
    if !on {
        return Held::Off;
    }
    let Some(pid) = mug else {
        return Held::Stopped { why: trouble };
    };
    match assertions_of(pid) {
        Ok(held)
            if assertion_types(&held, AWAKE_REASON)
                .iter()
                .any(|kind| kind == SYSTEM_ASSERTION) =>
        {
            Held::Held { pid }
        }
        Ok(_) => Held::NotHeld { pid, why: trouble },
        Err(why) => Held::Unread { pid, why },
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
    let mug = crate::instance::running_mug(&paths.lock_file(), &paths.code_dir())
        .map(|holder| holder.pid);
    let trouble = crate::trouble::read(&paths).map(|trouble| trouble.why);
    judge(on, mug, trouble, aterm_sysprobe::power::assertions_of)
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
}
