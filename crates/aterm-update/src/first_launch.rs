// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First launch installs aterm before it sets up (init-on-by-default design §6.8).
//!
//! When macOS opens the full aterm app from a disk image or an App Translocation copy,
//! nothing on that copy may set the Mac up or add `aterm` to PATH: the copy goes away. So on
//! such a first launch aterm installs the running bundle into Applications and relaunches from
//! the copy, opens an existing installed copy, or refuses — and never runs, nor clears
//! quarantine on, a copy before its signature, team pin ([`aterm_update_core::pins::APPLE_TEAM_ID`])
//! and notarization pass. An installed copy of the same or a newer version is verified the same
//! way before it is opened, so a planted unsigned bundle with a high `CFBundleVersion` is not
//! launched on its version alone; a copy that fails is moved aside. The original is quit only
//! once the copy shows it ran, by a line it wrote.
//!
//! Self-installation requires the full signed app supplied by the macOS DMG, ZIP and installer
//! routes. Bare development images do not self-install. A manually extracted product-release
//! executable without its signed app bundle refuses setup.

use crate::which_copy::InstallPosture;
use std::path::{Path, PathBuf};

/// Native macOS installation and first-launch succession.
#[cfg(target_os = "macos")]
pub mod native;

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
    if let Some(installed) = &facts.installed
        && installed.at_least_running
        && installed.verified.is_ok()
    {
        return FirstLaunchPlan::OpenInstalled(installed.app_root.clone());
    }
    let Some(dest) = facts.install_dest() else {
        return FirstLaunchPlan::NoDestination;
    };
    if let Some(installed) = &facts.installed {
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

/// What preparation found at the final installation boundary.
pub enum CopyDisposition {
    /// The verified running bundle was installed.
    Installed,
    /// A newly observed, verified same-or-newer copy is retained and should be opened.
    UseExisting,
}

/// The side effects a first launch needs, injected so the decision is testable with fakes. Every
/// method that touches a bundle names the bundle it acts on.
pub trait FirstLaunchHost {
    /// This launch's facts (posture, destinations, any installed copy and its gate verdict).
    fn facts(&self) -> Facts;
    /// Verify `app`'s code signature against the team pin and its notarization — `verify.rs`'s
    /// gate. `Ok(())` means it passed.
    fn verify(&self, app: &Path) -> Result<(), String>;
    /// Copy and verify the running bundle in isolation before promoting it to `dest`.
    /// An existing `replace` is retained only after that candidate passed verification;
    /// a failed copy or gate must leave the existing installed copy untouched.
    fn install_copy(&self, dest: &Path, replace: Option<&Path>) -> Result<CopyDisposition, String>;
    /// Move `app` out of the way; returns where it went.
    fn move_aside(&self, app: &Path) -> Result<PathBuf, String>;
    /// Clear `app`'s `com.apple.quarantine` — called only after [`FirstLaunchHost::verify`]
    /// passed on it.
    fn clear_quarantine(&self, app: &Path) -> Result<(), String>;
    /// Relaunch from `app` and return once it has shown it ran, by a line it wrote. `Ok(())`
    /// means the successor is up; the original may now quit. Recheck its authenticity at
    /// native dispatch, including an existing copy admitted by the initial facts.
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
            // probe, and the native dispatch rechecks it before execution.
            Ok(()) => FirstLaunchOutcome::Relaunched { from: app },
            Err(reason) => FirstLaunchOutcome::Refused {
                reason: open_refusal(&app, &reason),
            },
        },
        FirstLaunchPlan::Install { dest, move_aside } => {
            match host.install_copy(&dest, move_aside.as_deref()) {
                Err(reason) => {
                    return FirstLaunchOutcome::Refused {
                        reason: install_refusal(&dest, &reason),
                    };
                }
                Ok(CopyDisposition::UseExisting) => {
                    return match host.relaunch(&dest) {
                        Ok(()) => FirstLaunchOutcome::Relaunched { from: dest },
                        Err(reason) => FirstLaunchOutcome::Refused {
                            reason: open_refusal(&dest, &reason),
                        },
                    };
                }
                Ok(CopyDisposition::Installed) => {}
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
#[path = "first_launch_tests.rs"]
mod tests;
