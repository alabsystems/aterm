// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S YES, in a leaf module of its own (`native_settings::owner_yes`):
//! [`OwnerYes`], the two doors that mint one, the mint tests use, and the
//! alert's arm whose answer the first door reads ([`ConfirmArm`]). Nothing
//! else lives here.
//!
//! The compiler holds the yes to its doors. `OwnerYes` is a braced struct
//! whose one field is private to this module and of a private type
//! ([`YesSeal`]) that is neither `Clone`, `Copy` nor `Default`. A private field
//! is visible in its module and that module's children, and this module has
//! no children, so nothing outside this file (`native_settings` and its other
//! `#[path]` children included) can build or copy one in safe Rust, and no
//! `impl Copy for OwnerYes` compiles anywhere. Being braced, it has no
//! constructor function to pass around as a value.
//!
//! The aterm-census guard `devtools-door` holds this file to that shape: no
//! `mod`, no macro defined here and none invoked but by a path from the extern
//! prelude (`::aterm_log::info!`), no alias, glob import, `unsafe`, trait impl
//! for the yes, or attribute that could expand into code, and a `YesSeal` value
//! only inside a door's body. Outside this file no line names `YesSeal` (a
//! macro's body that did would expand here with this file's privacy), and the
//! census looks for the unsafe reads that could forge a yes and that no type
//! can stop. No shipped line here holds a function type (`fn(..)`, `Fn(..)`),
//! and [`ConfirmArm`] holds two unit variants and a stand-in under
//! `#[cfg(test)]`: the alert's arm takes no answer from its caller but in a
//! test build. The door asks it as `ConfirmArm::ask(gestures.confirm, ..)`, so
//! the gestures' arm is this type whatever `ConsentGestures` declares.

use super::{ConsentGestures, OwnerAnswer, QuietStamp, SharedOwnerQuiet};
use crate::workstation_setup::AdmittedAccess;

/// The owner's yes to a macOS access change. Two doors mint one, both in this
/// file:
///
/// * [`owner_confirms_with`], after its gestures' [`ConfirmArm`] raised aterm's
///   alert and the owner chose its proceed button: the Security page's presses
///   (`SettingsViewState::owner_confirms`), whose alerts stay.
/// * [`Self::from_admitted`], from the window's own admission of the setup plan
///   (`docs/DESIGN-system-state-2026-10-21.md` §6, "The standing yes"; owner,
///   2026-10-21: "AUTO APPROVED BY DEFAULT!!!! no user required"): System
///   State's once-per-Mac debugger step (`DebuggerChangeLane::start_from_plan`)
///   and a person's `Turn On…` press on its Debugger row
///   (`App::turn_on_debugger_from_system_state`). It supersedes owner ruling
///   O29's launch alert on the launch path.
///
/// The debugger-access change is built on it all the way down: only
/// `DebuggerChangeLane::begin` mints a `debugger_access::ChangeTicket`, and only
/// from a yes; a `DevtoolsJob` — what the worker that runs `DevToolsSecurity`
/// takes — is built only from a ticket. So no control verb, palette entry or
/// band action in this crate can start the change without the owner's yes in
/// an alert or the window's own admission, which no control client's gesture
/// makes (`workstation_setup::AdmittedAccess`, minted in a leaf of its own the
/// same way). The containment runner beneath it,
/// `aterm_containment::consent::run_devtools_change`, is `pub` across the crate
/// boundary; the census keeps it to its one caller,
/// `debugger_access::spawn_devtools_worker`. Neither `Clone` nor `Copy`: one yes
/// is spent by one change. The sudo preference writer likewise consumes one
/// before changing its marker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OwnerYes {
    _seal: YesSeal,
}

/// What makes an [`OwnerYes`] one yes: private to this file, so only this file
/// can make one, and neither `Clone`, `Copy` nor `Default`, so no copy of a
/// yes compiles.
#[derive(Debug, PartialEq, Eq)]
struct YesSeal;

impl OwnerYes {
    /// THE STANDING YES, spent: the window's own admission of a setup plan
    /// with its Permissions group (design §6). The token is consumed here; only
    /// `workstation_setup::PermissionGate::admitted` makes one, and never from a
    /// control client's gesture, a status poll or a hand-edited record.
    pub(crate) fn from_admitted(admitted: AdmittedAccess) -> Self {
        ::aterm_log::info!(
            "macOS access: the window's own admission of setup revision {} ({:?}) is the yes",
            admitted.revision(),
            admitted.by()
        );
        Self { _seal: YesSeal }
    }

    /// A yes minted outside an alert, for tests only.
    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { _seal: YesSeal }
    }
}

/// Who answers the owner's alert before a gesture that changes this Mac: the
/// confirm arm of `ConsentGestures`, whose answer [`owner_confirms_with`]
/// turns into a yes. aterm's own alert, or nobody; a test's stand-in exists
/// only in a test build. So no shipped line can arm the Security page's
/// gestures with an answer of its own (a closure, a fn that says yes): a yes
/// from the alert is the owner's press on its proceed button.
#[derive(Clone, Copy)]
pub(crate) enum ConfirmArm {
    /// aterm's AppKit alert (`menu::confirm_owner`): a windowed instance's.
    Alert,
    /// Nobody is asked: a headless instance's, and every unit test's default.
    Nobody,
    /// A test's stand-in for the owner.
    #[cfg(test)]
    StandIn(fn(&str, &str, &str) -> Option<bool>),
}

impl ConfirmArm {
    /// Ask through this arm: the owner's answer, or `None` when nobody could
    /// be asked (no alert could be shown), which is not a decline.
    pub(super) fn ask(self, title: &str, body: &str, proceed: &str) -> Option<bool> {
        match self {
            Self::Alert => alert(title, body, proceed),
            Self::Nobody => None,
            #[cfg(test)]
            Self::StandIn(answer) => answer(title, body, proceed),
        }
    }
}

/// aterm's alert: AppKit on macOS; off macOS nobody is asked (the Security
/// block is never published there).
fn alert(title: &str, body: &str, proceed: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::menu::confirm_owner(title, body, proceed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, body, proceed);
        None
    }
}

/// Ask the owner in aterm's alert, through `gestures`' confirm arm, unless a
/// recent decline quiets every Security gesture (`quiet`): the one place an
/// alert mints an [`OwnerYes`]. A yes ends the quiet, a no starts or doubles
/// it, and an alert that could not be shown is [`OwnerAnswer::NotAsked`],
/// never a yes.
pub(crate) fn owner_confirms_with(
    quiet: &SharedOwnerQuiet,
    gestures: &ConsentGestures,
    title: &str,
    body: &str,
    proceed: &str,
) -> OwnerAnswer {
    if quiet
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .quiet_at(QuietStamp::now())
    {
        return OwnerAnswer::Quiet;
    }
    // No lock is held while the alert is up: it runs a modal loop. Asked
    // through `ConfirmArm`'s own `ask`, so the arm the gestures hold is this
    // file's `ConfirmArm`, or nothing compiles.
    let answer = ConfirmArm::ask(gestures.confirm, title, body, proceed);
    let mut quiet = quiet.lock().unwrap_or_else(|p| p.into_inner());
    match answer {
        Some(true) => {
            quiet.accepted();
            OwnerAnswer::Yes(OwnerYes { _seal: YesSeal })
        }
        Some(false) => {
            quiet.declined(QuietStamp::now());
            OwnerAnswer::No
        }
        None => OwnerAnswer::NotAsked,
    }
}
