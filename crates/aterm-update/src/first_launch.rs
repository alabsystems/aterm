// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First launch installs aterm before it sets up (init-on-by-default design §6.8).
//!
//! When macOS opens aterm from a disk image, an App Translocation copy or a plain binary,
//! nothing on that copy may set the Mac up or add `aterm` to PATH: the copy goes away. So on
//! such a first launch aterm installs the running bundle into Applications and relaunches from
//! the copy, opens an existing installed copy, or refuses — and never runs, nor clears
//! quarantine on, a copy before its signature, team pin ([`aterm_update_core::pins::APPLE_TEAM_ID`])
//! and notarization pass. An installed copy of the same or a newer version is verified the same
//! way before it is opened, so a planted unsigned bundle with a high `CFBundleVersion` is not
//! launched on its version alone; a copy that fails is moved aside. The original is quit only
//! once the copy shows it ran, by a line it wrote.

use crate::which_copy::InstallPosture;
use std::path::{Path, PathBuf};

/// Whether setup, the subscriber, Dock pins and login items may run from `posture`. Only an
/// installed copy lasts; every other copy goes away, so none of those run there. A platform
/// without a bundle posture is not gated by this (its caller passes [`InstallPosture::Installed`]
/// or short-circuits), matching the design's "the base sync plan reaches every Mac".
#[must_use]
pub const fn setup_runs_from_posture(posture: InstallPosture) -> bool {
    matches!(posture, InstallPosture::Installed)
}

/// An aterm already present where the running copy would land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledCopy {
    /// Its `.app` root.
    pub app_root: PathBuf,
    /// Its `CFBundleVersion` is the same as, or newer than, the running copy's. A copy with a
    /// high version is still verified before it is opened — the version alone opens nothing.
    pub at_least_running: bool,
    /// The team-pin-and-notarization gate's verdict on it
    /// (`verify::check_bundle(_, effective_team_id(), _).policy`).
    pub verified: Result<(), String>,
}

/// The facts a [`plan`] rests on.
#[derive(Clone, Debug)]
pub struct Facts {
    /// The running copy's posture.
    pub posture: InstallPosture,
    /// Whether the user can write `/Applications` (effectively, whether they are an admin): the
    /// copy lands there when they can, else in `~/Applications`.
    pub can_write_applications: bool,
    /// `/Applications/aterm.app`.
    pub applications_dest: PathBuf,
    /// `~/Applications/aterm.app`, when `HOME` is known.
    pub home_dest: Option<PathBuf>,
    /// An aterm already installed in a usual place, or `None` when none was found.
    pub installed: Option<InstalledCopy>,
}

impl Facts {
    /// Where the running copy would be installed: `/Applications` for a user who can write it,
    /// else `~/Applications`. `None` only when neither is writable (no `HOME`).
    #[must_use]
    pub fn install_dest(&self) -> Option<PathBuf> {
        if self.can_write_applications {
            Some(self.applications_dest.clone())
        } else {
            self.home_dest.clone()
        }
    }
}

/// What a first launch should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstLaunchPlan {
    /// Already installed; setup runs here and nothing is copied.
    AlreadyInstalled,
    /// Open the existing installed copy at this path — it verified — and copy nothing.
    OpenInstalled(PathBuf),
    /// Install the running bundle to `dest` and relaunch from it. `move_aside` names a copy at
    /// the destination to move out of the way first (an existing one that is not opened);
    /// `None` when the destination is free.
    Install {
        /// Where the copy lands.
        dest: PathBuf,
        /// A copy sitting at `dest` to move aside before the copy lands.
        move_aside: Option<PathBuf>,
    },
    /// No writable destination: nothing can be installed. Keep the move-to-Applications remedy;
    /// setup does not run.
    NoDestination,
}

/// Decide a first launch from the facts. Pure: the verdicts in `facts.installed` already carry
/// the team-pin-and-notarization gate's answer, so this never opens an unverified copy.
#[must_use]
pub fn plan(facts: &Facts) -> FirstLaunchPlan {
    if facts.posture == InstallPosture::Installed {
        return FirstLaunchPlan::AlreadyInstalled;
    }
    let Some(dest) = facts.install_dest() else {
        return FirstLaunchPlan::NoDestination;
    };
    if let Some(installed) = &facts.installed {
        // Same-or-newer and verified: open it, copy nothing. A high version with a failed gate
        // falls through to Install, so it is never opened on its version alone.
        if installed.at_least_running && installed.verified.is_ok() {
            return FirstLaunchPlan::OpenInstalled(installed.app_root.clone());
        }
        // An existing copy sitting where ours would land is moved aside first; one elsewhere is
        // simply left where it is (we install to `dest` and do not open it).
        let move_aside = (installed.app_root == dest).then(|| installed.app_root.clone());
        return FirstLaunchPlan::Install { dest, move_aside };
    }
    FirstLaunchPlan::Install {
        dest,
        move_aside: None,
    }
}

/// What a first launch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstLaunchOutcome {
    /// Running from an installed copy already: the caller proceeds with setup here.
    ProceedInstalled,
    /// Relaunched from `from` (a freshly installed copy, or a verified existing one). The caller
    /// must quit this inert instance now — the successor wrote its proof line.
    Relaunched {
        /// The installed copy now running.
        from: PathBuf,
    },
    /// Could not install, verify or relaunch. `reason` is the one row to file; the
    /// move-to-Applications remedy stays. Setup does not run here.
    Refused {
        /// Why, in one line.
        reason: String,
    },
}

/// The side effects a first launch needs, injected so the decision is testable with fakes. Every
/// method that touches a bundle names the bundle it acts on.
pub trait FirstLaunchHost {
    /// This launch's facts (posture, destinations, any installed copy and its gate verdict).
    fn facts(&self) -> Facts;
    /// Verify `app`'s code signature against the team pin and its notarization — `verify.rs`'s
    /// gate. `Ok(())` means it passed.
    fn verify(&self, app: &Path) -> Result<(), String>;
    /// Copy the running bundle to `dest` (a complete, isolated-then-promoted copy).
    fn install_copy(&self, dest: &Path) -> Result<(), String>;
    /// Move `app` out of the way; returns where it went.
    fn move_aside(&self, app: &Path) -> Result<PathBuf, String>;
    /// Clear `app`'s `com.apple.quarantine` — called only after [`FirstLaunchHost::verify`]
    /// passed on it.
    fn clear_quarantine(&self, app: &Path) -> Result<(), String>;
    /// Relaunch from `app` and return once it has shown it ran, by a line it wrote. `Ok(())`
    /// means the successor is up; the original may now quit.
    fn relaunch(&self, app: &Path) -> Result<(), String>;
}

/// Install the running bundle and relaunch from it, following [`plan`]. Never clears quarantine
/// on, nor relaunches, a copy whose gate did not pass; a copy that fails is moved aside; the
/// original is reported quittable ([`FirstLaunchOutcome::Relaunched`]) only once the successor
/// wrote its proof line.
pub fn install_and_relaunch<H: FirstLaunchHost>(host: &H) -> FirstLaunchOutcome {
    let facts = host.facts();
    match plan(&facts) {
        FirstLaunchPlan::AlreadyInstalled => FirstLaunchOutcome::ProceedInstalled,
        FirstLaunchPlan::OpenInstalled(app) => match host.relaunch(&app) {
            // The plan reaches this arm only for a same-or-newer copy whose gate passed in the
            // probe, so no copy is made and nothing unverified is opened.
            Ok(()) => FirstLaunchOutcome::Relaunched { from: app },
            Err(reason) => FirstLaunchOutcome::Refused {
                reason: open_refusal(&app, &reason),
            },
        },
        FirstLaunchPlan::Install { dest, move_aside } => {
            if let Some(existing) = move_aside
                && let Err(reason) = host.move_aside(&existing)
            {
                return FirstLaunchOutcome::Refused {
                    reason: install_refusal(&dest, &reason),
                };
            }
            if let Err(reason) = host.install_copy(&dest) {
                return FirstLaunchOutcome::Refused {
                    reason: install_refusal(&dest, &reason),
                };
            }
            // Verify the copy before clearing quarantine or running it. A copy that fails is
            // moved aside, and the move-to-Applications remedy stays with the reason.
            if let Err(reason) = host.verify(&dest) {
                let _ = host.move_aside(&dest);
                return FirstLaunchOutcome::Refused {
                    reason: verify_refusal(&dest, &reason),
                };
            }
            if let Err(reason) = host.clear_quarantine(&dest) {
                return FirstLaunchOutcome::Refused {
                    reason: install_refusal(&dest, &reason),
                };
            }
            match host.relaunch(&dest) {
                Ok(()) => FirstLaunchOutcome::Relaunched { from: dest },
                Err(reason) => FirstLaunchOutcome::Refused {
                    reason: open_refusal(&dest, &reason),
                },
            }
        }
        FirstLaunchPlan::NoDestination => FirstLaunchOutcome::Refused {
            reason: "aterm could not install itself: no writable Applications folder. Move \
                     aterm.app to Applications and open it from there."
                .to_string(),
        },
    }
}

fn install_refusal(dest: &Path, why: &str) -> String {
    format!(
        "aterm could not install itself to {}: {why}. Move aterm.app to Applications and open \
         it from there.",
        dest.display()
    )
}

fn verify_refusal(dest: &Path, why: &str) -> String {
    format!(
        "aterm refused to run the copy it placed at {} because it did not verify: {why}. The \
         copy was moved aside. Move a trusted aterm.app to Applications and open it from there.",
        dest.display()
    )
}

fn open_refusal(app: &Path, why: &str) -> String {
    format!(
        "aterm installed itself to {} but could not confirm it started: {why}. Open aterm from \
         Applications.",
        app.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn applications() -> PathBuf {
        PathBuf::from("/Applications/aterm.app")
    }

    fn home_applications() -> PathBuf {
        PathBuf::from("/Users//p/Applications/aterm.app")
    }

    #[derive(Default)]
    struct Calls {
        installed_to: Vec<PathBuf>,
        verified: Vec<PathBuf>,
        moved_aside: Vec<PathBuf>,
        cleared: Vec<PathBuf>,
        relaunched: Vec<PathBuf>,
    }

    /// A recording host whose seams all succeed unless a verdict is set.
    struct Fake {
        facts: Facts,
        /// `verify` returns this for every path (the copy and any installed probe share a fate
        /// in these cases; the installed probe's own verdict lives in `facts.installed`).
        verify: Result<(), String>,
        relaunch: Result<(), String>,
        calls: RefCell<Calls>,
    }

    impl Fake {
        fn new(posture: InstallPosture, admin: bool) -> Self {
            Self {
                facts: Facts {
                    posture,
                    can_write_applications: admin,
                    applications_dest: applications(),
                    home_dest: Some(home_applications()),
                    installed: None,
                },
                verify: Ok(()),
                relaunch: Ok(()),
                calls: RefCell::new(Calls::default()),
            }
        }
    }

    impl FirstLaunchHost for Fake {
        fn facts(&self) -> Facts {
            self.facts.clone()
        }
        fn verify(&self, app: &Path) -> Result<(), String> {
            self.calls.borrow_mut().verified.push(app.to_path_buf());
            self.verify.clone()
        }
        fn install_copy(&self, dest: &Path) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .installed_to
                .push(dest.to_path_buf());
            Ok(())
        }
        fn move_aside(&self, app: &Path) -> Result<PathBuf, String> {
            self.calls.borrow_mut().moved_aside.push(app.to_path_buf());
            Ok(app.with_extension("app.moved"))
        }
        fn clear_quarantine(&self, app: &Path) -> Result<(), String> {
            self.calls.borrow_mut().cleared.push(app.to_path_buf());
            Ok(())
        }
        fn relaunch(&self, app: &Path) -> Result<(), String> {
            self.calls.borrow_mut().relaunched.push(app.to_path_buf());
            self.relaunch.clone()
        }
    }

    #[test]
    fn setup_runs_only_from_an_installed_copy() {
        assert!(setup_runs_from_posture(InstallPosture::Installed));
        for posture in [
            InstallPosture::MountedImage,
            InstallPosture::Translocated,
            InstallPosture::NotABundle,
        ] {
            assert!(
                !setup_runs_from_posture(posture),
                "setup must not run from {posture:?}"
            );
        }
    }

    #[test]
    fn a_translocated_copy_installs_to_applications_for_an_admin_then_relaunches() {
        let fake = Fake::new(InstallPosture::Translocated, true);
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert_eq!(calls.installed_to, vec![applications()]);
        assert_eq!(calls.verified, vec![applications()], "the copy is verified");
        assert_eq!(
            calls.cleared,
            vec![applications()],
            "quarantine cleared after verify"
        );
        assert_eq!(calls.relaunched, vec![applications()]);
        assert_eq!(
            outcome,
            FirstLaunchOutcome::Relaunched {
                from: applications()
            }
        );
    }

    #[test]
    fn a_translocated_copy_installs_to_home_applications_for_a_non_admin() {
        let fake = Fake::new(InstallPosture::Translocated, false);
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert_eq!(calls.installed_to, vec![home_applications()]);
        assert_eq!(calls.relaunched, vec![home_applications()]);
        assert_eq!(
            outcome,
            FirstLaunchOutcome::Relaunched {
                from: home_applications()
            }
        );
    }

    /// The control for the two above: an installed copy copies nothing and relaunches nothing.
    #[test]
    fn an_installed_copy_installs_nothing_and_proceeds() {
        let fake = Fake::new(InstallPosture::Installed, true);
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert!(calls.installed_to.is_empty(), "nothing copied");
        assert!(calls.relaunched.is_empty(), "nothing relaunched");
        assert_eq!(outcome, FirstLaunchOutcome::ProceedInstalled);
    }

    #[test]
    fn a_copy_that_fails_the_gate_is_moved_aside_with_no_relaunch_and_no_setup() {
        let mut fake = Fake::new(InstallPosture::Translocated, true);
        fake.verify = Err(
            "Team ID mismatch: bundle is signed by \"XXXX\", expected \"A66A9P66Z7\"".to_string(),
        );
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert_eq!(calls.installed_to, vec![applications()]);
        assert_eq!(calls.verified, vec![applications()]);
        assert!(
            calls.cleared.is_empty(),
            "quarantine never cleared on an unverified copy"
        );
        assert!(calls.relaunched.is_empty(), "no relaunch");
        assert_eq!(
            calls.moved_aside,
            vec![applications()],
            "the failed copy is moved aside"
        );
        match outcome {
            FirstLaunchOutcome::Refused { reason } => {
                assert!(reason.contains("Team ID mismatch"), "names why: {reason}");
                assert!(
                    reason.contains("A66A9P66Z7"),
                    "names the expected team: {reason}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_newer_installed_but_unverified_copy_is_not_opened() {
        let mut fake = Fake::new(InstallPosture::Translocated, true);
        fake.facts.installed = Some(InstalledCopy {
            app_root: applications(),
            at_least_running: true,
            verified: Err("spctl: rejected (no usable signature)".to_string()),
        });
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        // The planted copy sits at the destination, so it is moved aside; our verified copy lands
        // and is opened — the planted one is never relaunched.
        assert_eq!(calls.moved_aside, vec![applications()]);
        assert_eq!(calls.installed_to, vec![applications()]);
        assert_eq!(
            calls.relaunched,
            vec![applications()],
            "only our copy is opened"
        );
        assert_eq!(
            outcome,
            FirstLaunchOutcome::Relaunched {
                from: applications()
            }
        );
    }

    /// The control: a verified same-or-newer copy is opened and nothing is copied.
    #[test]
    fn a_verified_newer_installed_copy_is_opened_and_nothing_is_copied() {
        let mut fake = Fake::new(InstallPosture::Translocated, true);
        fake.facts.installed = Some(InstalledCopy {
            app_root: applications(),
            at_least_running: true,
            verified: Ok(()),
        });
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert!(
            calls.installed_to.is_empty(),
            "nothing copied over a verified newer copy"
        );
        assert!(
            calls.moved_aside.is_empty(),
            "the verified copy is left in place"
        );
        assert_eq!(calls.relaunched, vec![applications()]);
        assert_eq!(
            outcome,
            FirstLaunchOutcome::Relaunched {
                from: applications()
            }
        );
    }

    /// A newer installed-but-unverified copy that a non-admin cannot move (it is at
    /// `/Applications`, our destination is `~/Applications`) is left where it is, not opened, and
    /// our copy lands in `~/Applications`.
    #[test]
    fn an_unverified_copy_elsewhere_is_left_alone_and_not_opened() {
        let mut fake = Fake::new(InstallPosture::Translocated, false);
        fake.facts.installed = Some(InstalledCopy {
            app_root: applications(),
            at_least_running: true,
            verified: Err("rejected".to_string()),
        });
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert!(
            calls.moved_aside.is_empty(),
            "a copy elsewhere is not moved by a non-admin"
        );
        assert_eq!(calls.installed_to, vec![home_applications()]);
        assert_eq!(calls.relaunched, vec![home_applications()]);
        assert_eq!(
            outcome,
            FirstLaunchOutcome::Relaunched {
                from: home_applications()
            }
        );
    }

    #[test]
    fn a_relaunch_that_never_shows_it_ran_refuses_and_does_not_claim_the_copy_started() {
        let mut fake = Fake::new(InstallPosture::Translocated, true);
        fake.relaunch =
            Err("the copy did not write its proof line within the deadline".to_string());
        let outcome = install_and_relaunch(&fake);
        match outcome {
            FirstLaunchOutcome::Refused { reason } => {
                assert!(reason.contains("could not confirm it started"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn no_writable_destination_refuses_and_keeps_the_move_remedy() {
        let mut fake = Fake::new(InstallPosture::Translocated, false);
        fake.facts.home_dest = None;
        let outcome = install_and_relaunch(&fake);
        let calls = fake.calls.borrow();
        assert!(calls.installed_to.is_empty());
        match outcome {
            FirstLaunchOutcome::Refused { reason } => {
                assert!(
                    reason.contains("Move aterm.app to Applications"),
                    "{reason}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
