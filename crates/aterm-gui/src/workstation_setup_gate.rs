// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's own admission, in a leaf module of its own (`workstation_setup::permission_gate`):
//! the gate ([`PermissionGate`]) that reads who touched System State and which plan revision the
//! window's own Apply admitted.
//!
//! The gate's fields are private to this module, and this module has no children, so nothing
//! outside this file (`workstation_setup` included) can set the gate's record of the window's own
//! admission but through its transitions, which are `pub(super)`: callable from
//! `workstation_setup` alone. The aterm-census guard `devtools-door` holds those transitions to
//! their one App caller each there.

use super::{AdmittedBy, PagePress};

#[derive(Default)]
enum PermissionHold {
    #[default]
    None,
    Decision,
    FreshStatus,
    /// Stop withdraws automatic work locally.
    Stopped,
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
        // Stop.
        self.hold = if ::std::matches!(self.hold, PermissionHold::Stopped) {
            PermissionHold::Stopped
        } else {
            PermissionHold::Decision
        };
        // A newer mutating intent supersedes the window's own admission until
        // its own acceptance (if it is the window's) names a revision again.
        self.own = None;
        self.accepted_by = None;
    }

    pub(super) fn stop(&mut self) {
        self.hold = PermissionHold::Stopped;
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

    /// The Apply being admitted came from a person's press.
    pub(super) const fn apply_is_a_persons(&self) -> bool {
        ::std::matches!(self.apply_by, Some(AdmittedBy::Person))
    }

    /// A person's press on the page's offer asked for macOS's grants: the status read after its
    /// record releases them with this press as the window's own admission. A Stop stays a Stop.
    pub(super) fn person_asked(&mut self) {
        if !::std::matches!(self.hold, PermissionHold::Stopped) {
            self.accepted_by = Some(AdmittedBy::Person);
            self.hold = PermissionHold::FreshStatus;
        }
    }

    pub(super) fn await_fresh_status(&mut self) {
        if !::std::matches!(self.hold, PermissionHold::Stopped) {
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

    /// Whether the selected plan is this window's own admission. An automatic admission
    /// remains a valid setup decision without granting a permission-prompt capability.
    pub(super) fn own_admission_stands(&self) -> bool {
        self.allowed()
            && self
                .policy
                .zip(self.own)
                .is_some_and(|(policy, (_, revision))| revision == policy.revision)
    }

    /// Whether this revision has an explicit person's permission choice.
    pub(super) fn person_admission_stands(&self) -> bool {
        self.own_admission_stands() && self.own.is_some_and(|(by, _)| by == AdmittedBy::Person)
    }
}
