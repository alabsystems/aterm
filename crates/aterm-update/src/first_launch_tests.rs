// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    fn install_copy(&self, dest: &Path, replace: Option<&Path>) -> Result<CopyDisposition, String> {
        self.calls
            .borrow_mut()
            .installed_to
            .push(dest.to_path_buf());
        if let Some(replace) = replace {
            self.move_aside(replace)?;
        }
        Ok(CopyDisposition::Installed)
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
    fake.verify =
        Err("Team ID mismatch: bundle is signed by \"XXXX\", expected \"A66A9P66Z7\"".to_string());
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
    fake.facts.can_write_applications = false;
    fake.facts.home_dest = None;
    assert_eq!(
        plan(&fake.facts),
        FirstLaunchPlan::OpenInstalled(applications()),
        "reopening the verified app requires no installation destination"
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
    fake.relaunch = Err("the copy did not write its proof line within the deadline".to_string());
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
