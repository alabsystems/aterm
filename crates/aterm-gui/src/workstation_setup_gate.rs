// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's own admission, in a leaf module of its own (`workstation_setup::permission_gate`):
//! the gate ([`PermissionGate`]) that reads who touched System State and which plan revision the
//! window's own Apply admitted, and the token it alone mints ([`AdmittedAccess`]).
//!
//! The compiler holds the token to its door. `AdmittedAccess` is a braced struct whose fields are
//! private to this module, one of them of a private type (`AdmissionSeal`) that is neither
//! `Clone`, `Copy` nor `Default`, and the gate's fields are private too. A private field is
//! visible only in its module and that module's children, and this module has none, so nothing
//! outside this file (`workstation_setup` included) can build or copy an admission in safe Rust,
//! or set the gate's record of the window's own admission but through its transitions.
//!
//! The aterm-census guard `devtools-door` holds this file to that shape: no `mod`, no macro
//! defined here and none invoked but by an extern-prelude path (`::std::matches!`), no alias, glob
//! import, `unsafe`, trait impl for the token, or attribute that could expand into code, and an
//! `AdmissionSeal` value only inside `PermissionGate::admitted` and the test mint. Outside this
//! file no line may name `AdmissionSeal` (a macro body that did would expand here with this
//! file's privacy), and the census flags the unsafe reads that could forge an admission, which
//! no type can stop.

use super::{AdmittedBy, PagePress};

#[derive(Default)]
enum PermissionHold {
    #[default]
    None,
    Decision,
    FreshStatus,
    /// Stop withdraws automatic work locally. Only a later person's own
    /// row press may still use an otherwise known admitted policy.
    Stopped {
        person: bool,
    },
}

/// The standing yes (design §6): proof that this window itself admitted a setup plan with its
/// Permissions group, and of which revision. Only [`PermissionGate::admitted`] makes one, for the
/// window's own admission (its automatic answer to the startup offer, or a person's press on the
/// page) or for a person's press on a row's own button. A control client's `act` or `key`, a
/// status poll and a hand-edited plan record never make one: only the window's own Apply sets the
/// gate's record of its admission ([`PermissionGate::observe`] after
/// [`PermissionGate::accepted`]).
///
/// `crate::native_settings::OwnerYes::from_admitted` spends one as the yes a debugger-access
/// change needs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AdmittedAccess {
    revision: u64,
    by: AdmittedBy,
    _seal: AdmissionSeal,
}

/// What makes an [`AdmittedAccess`] one admission: private to this file, and neither `Clone`,
/// `Copy` nor `Default`.
#[derive(Debug, PartialEq, Eq)]
struct AdmissionSeal;

impl AdmittedAccess {
    /// The setup plan revision the admission is of: the access record's key
    /// (`aterm_types::macos_setup::access_record`).
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    /// Who the admission came from.
    pub(crate) const fn by(&self) -> AdmittedBy {
        self.by
    }

    /// A token minted outside the gate (test-only).
    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) const fn for_test(revision: u64) -> Self {
        Self {
            revision,
            by: AdmittedBy::Window,
            _seal: AdmissionSeal,
        }
    }
}

/// What the window saw of who touched System State since its previous action
/// was taken ([`PermissionGate::take_press`]).
#[derive(Default)]
struct PressEvidence {
    /// The App's count of control-client gestures (any view) when the evidence
    /// was last taken. A gesture counted since taints the next action, so a
    /// control client's `act`, `key`, `text` or `pointer` is never a person's
    /// press, whatever path it took into the page.
    controller_seen: u64,
    /// A gesture that was not a control client's reached the page since.
    person: bool,
    /// A control client's gesture reached the page itself since.
    controller_touched: bool,
}

#[derive(Default)]
pub(super) struct PermissionGate {
    policy: Option<crate::InitialSetupPermissionPolicy>,
    latest_revision: u64,
    hold: PermissionHold,
    evidence: PressEvidence,
    /// Who the Apply now being admitted came from; `None`: a control client,
    /// or no evidence of the window's own.
    apply_by: Option<AdmittedBy>,
    /// An accepted Apply's provenance, waiting for the fresh status that
    /// releases the hold and names its revision.
    accepted_by: Option<AdmittedBy>,
    /// The window's own admission: who, and the revision it admitted. Cleared by every newer
    /// mutating intent and by any status naming another plan.
    own: Option<(AdmittedBy, u64)>,
}

impl PermissionGate {
    /// Whether setup is free of a local decision or Stop.
    /// The caller supplies the separate exact run's durable authority.
    pub(super) fn setup_unheld(&self) -> bool {
        ::std::matches!(self.hold, PermissionHold::None)
    }

    pub(super) fn allowed(&self) -> bool {
        ::std::matches!(self.hold, PermissionHold::None)
            && self.policy.is_some_and(|policy| policy.allowed)
    }

    /// The approved setup revision whose access steps may run: the admitted
    /// plan's revision while the gate allows Permissions, else none.
    pub(super) fn access_revision(&self) -> Option<u64> {
        self.policy
            .filter(|_| self.allowed())
            .map(|policy| policy.revision)
    }

    pub(super) fn hold(&mut self) {
        // An unanswered offer or refused intent cannot withdraw the earlier
        // Stop. The new decision also keeps a person's row override held.
        self.hold = if ::std::matches!(self.hold, PermissionHold::Stopped { .. }) {
            PermissionHold::Stopped { person: false }
        } else {
            PermissionHold::Decision
        };
        // A newer mutating intent supersedes the window's own admission until
        // its own acceptance (if it is the window's) names a revision again.
        self.own = None;
        self.accepted_by = None;
    }

    pub(super) fn stop(&mut self) {
        let person = ::std::matches!(
            self.hold,
            PermissionHold::None | PermissionHold::Stopped { person: true }
        ) && self.policy.is_some_and(|policy| policy.allowed);
        self.hold = PermissionHold::Stopped { person };
        self.own = None;
        self.accepted_by = None;
    }

    pub(super) fn accepted(&mut self) {
        // The receipt establishes durable admission, but the cached snapshot
        // can still describe the previous plan. Only a new read releases it.
        self.accepted_by = self.apply_by.take();
        // The host's post-Stop receipt fence admits this new choice; an
        // ordinary refresh never releases the earlier Stop by itself.
        self.hold = PermissionHold::FreshStatus;
    }

    pub(super) fn await_fresh_status(&mut self) {
        if !::std::matches!(self.hold, PermissionHold::Stopped { .. }) {
            self.hold = PermissionHold::FreshStatus;
        }
    }

    /// Take a newer status. `true` when it establishes the window's own
    /// admission of the plan it names.
    pub(super) fn observe(&mut self, policy: Option<crate::InitialSetupPermissionPolicy>) -> bool {
        let Some(policy) = policy else {
            self.policy = None;
            self.own = None;
            return false;
        };
        if policy.allowed && policy.revision < self.latest_revision {
            return false;
        }
        self.latest_revision = self.latest_revision.max(policy.revision);
        self.policy = Some(policy);
        let mut admitted = false;
        if ::std::matches!(self.hold, PermissionHold::FreshStatus) {
            self.hold = PermissionHold::None;
            if let Some(by) = self.accepted_by.take()
                && policy.allowed
            {
                self.own = Some((by, policy.revision));
                admitted = true;
            }
        }
        if self
            .own
            .is_some_and(|(_, revision)| !policy.allowed || revision != policy.revision)
        {
            self.own = None;
        }
        admitted
    }

    /// A gesture reached the System State page; `controller` when it was a
    /// control client's.
    pub(super) fn note_touch(&mut self, controller: bool) {
        if controller {
            self.evidence.controller_touched = true;
        } else {
            self.evidence.person = true;
        }
    }

    /// Whether a control client's gesture reached the page itself since the
    /// page's previous action was taken.
    pub(super) const fn controller_touched(&self) -> bool {
        self.evidence.controller_touched
    }

    /// Take the evidence for the page's action being handled now: a person's
    /// press only when a person touched the page and no control client's
    /// gesture (`controller_gestures`, the App's count) came since the
    /// previous take. Either way the evidence starts again.
    pub(super) fn take_press(&mut self, controller_gestures: u64) -> PagePress {
        let touched = std::mem::take(&mut self.evidence.controller_touched);
        let controller = touched || controller_gestures != self.evidence.controller_seen;
        self.evidence.controller_seen = controller_gestures;
        let person = std::mem::take(&mut self.evidence.person);
        if controller {
            PagePress::Controller
        } else if person {
            PagePress::Person
        } else {
            PagePress::Nobody
        }
    }

    /// The page's Apply is being handled: note who it came from. `offer` when
    /// it answers the startup offer. Nobody's gesture in an Apply that answers
    /// no offer is no evidence of the window's own, and counts as none.
    pub(super) fn apply_started(&mut self, offer: bool, press: PagePress) {
        self.apply_by = match press {
            PagePress::Controller => None,
            PagePress::Person => Some(AdmittedBy::Person),
            PagePress::Nobody => offer.then_some(AdmittedBy::Window),
        };
    }

    /// Whether the plan the status names (Permissions admitted, nothing held) is the window's own
    /// admission: what [`Self::admitted`] reads with no row press, as a fact that spends nothing.
    pub(super) fn own_admission_stands(&self) -> bool {
        self.allowed()
            && self
                .policy
                .zip(self.own)
                .is_some_and(|(policy, (_, revision))| revision == policy.revision)
    }

    /// The standing yes, while the plan the status names (Permissions admitted, nothing held) is
    /// the window's own admission; or, for `person_press` (a person's press on a row's own
    /// button), while that plan stands at all. The one door that makes an [`AdmittedAccess`].
    pub(super) fn admitted(&self, person_press: bool) -> Option<AdmittedAccess> {
        let stopped_press = person_press
            && ::std::matches!(self.hold, PermissionHold::Stopped { person: true })
            && self.policy.is_some_and(|policy| policy.allowed);
        if !self.allowed() && !stopped_press {
            return None;
        }
        let policy = self.policy?;
        if person_press {
            return Some(AdmittedAccess {
                revision: policy.revision,
                by: AdmittedBy::Person,
                _seal: AdmissionSeal,
            });
        }
        let (by, revision) = self.own?;
        (revision == policy.revision).then_some(AdmittedAccess {
            revision,
            by,
            _seal: AdmissionSeal,
        })
    }
}
