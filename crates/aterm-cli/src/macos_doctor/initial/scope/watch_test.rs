// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scratch-only production persistence for the binary's ALab watch regressions.

use super::*;

/// An inert completed provisional run at a caller-supplied scratch root. Its operations
/// use the real admission and record writers, with no account, network or preference ports.
pub struct ProvisionalSetupWatchTest<'a> {
    root: &'a Path,
    upgrade: InitialSetupUpgrade,
}

impl<'a> ProvisionalSetupWatchTest<'a> {
    /// Reopen the supplied scratch records without creating a new admission.
    pub fn at(root: &'a Path) -> Self {
        Self {
            root,
            upgrade: InitialSetupUpgrade::default(),
        }
    }

    pub fn new(root: &'a Path) -> Result<Self, String> {
        let narrow = scoped(Plan::default(), false);
        InitialSetupAuthorization::capture_at(root)?
            .persist_for_watch_test(root, &narrow, false, None)?
            .ok_or("the initial provisional admission was refused")?;
        let mut completed = progress(root)?.ok_or("the provisional run is missing")?;
        completed.done = Stage::Permissions.bit();
        completed.completed = true;
        save_progress(root, completed)?;
        Ok(Self {
            root,
            upgrade: InitialSetupUpgrade::default(),
        })
    }

    pub fn widens(&self) -> Result<bool, String> {
        if self.upgrade.pending() {
            Ok(true)
        } else {
            widens_at(self.root)
        }
    }

    pub fn widens_from(&self) -> Option<u64> {
        self.upgrade.widens_from()
    }

    pub fn upgrade_pending(&self) -> bool {
        self.upgrade.pending()
    }

    pub fn published_upgrade(&self) -> Result<Option<SetupUpgrade>, String> {
        self.upgrade.published_at(self.root)
    }

    pub fn forget_upgrade(&mut self) {
        self.upgrade.forget();
    }

    pub fn published_upgrade_after_failed_read(&self) -> Result<Option<SetupUpgrade>, String> {
        let path = self.root.join("initial-setup.json");
        let before = std::fs::read(&path).map_err(|error| error.to_string())?;
        std::fs::write(&path, b"{\"profile\":").map_err(|error| error.to_string())?;
        let result = self.published_upgrade();
        std::fs::write(&path, before).map_err(|error| error.to_string())?;
        result
    }

    pub fn lose_upgrade_publication(&mut self) -> Result<Option<SetupUpgrade>, String> {
        self.lose_upgrade_publication_at(0, true)
    }

    /// Lose publication identity before or after one real ordered write. Earlier
    /// successful checkpoints retain their own Stop evidence, never write authority.
    pub fn lose_upgrade_publication_at(
        &mut self,
        at: usize,
        published: bool,
    ) -> Result<Option<SetupUpgrade>, String> {
        self.upgrade.persist_with(
            self.root,
            || true,
            || Ok(None),
            |ticket, plan| ticket.lose_publication_for_watch_test(self.root, plan, at, published),
        )
    }

    /// A failed eligibility read repaired before control returns to the watch. This is
    /// a real parse failure; it neither changes admission nor contacts any external port.
    pub fn widens_after_failed_read(&self) -> Result<bool, String> {
        let path = self.root.join("initial-setup.json");
        let before = std::fs::read(&path).map_err(|error| error.to_string())?;
        std::fs::write(&path, b"{\"profile\":").map_err(|error| error.to_string())?;
        let observed = self.widens();
        std::fs::write(&path, &before).map_err(|error| error.to_string())?;
        if std::fs::read(&path).map_err(|error| error.to_string())? != before {
            return Err("the repaired provisional progress changed".into());
        }
        observed
    }

    /// Upgrade under the real profile lock, optionally failing before or after one actual
    /// record publication. The indexes are the writer's existing ordered admission steps.
    pub fn upgrade(&mut self, fault: Option<(usize, bool)>) -> Result<Option<u64>, String> {
        self.upgrade_outcome(fault)
            .map(|upgrade| upgrade.map(|upgrade| upgrade.to))
    }

    /// The exact predecessor and target produced by the real admission.
    pub fn upgrade_outcome(
        &mut self,
        fault: Option<(usize, bool)>,
    ) -> Result<Option<SetupUpgrade>, String> {
        self.upgrade.persist_with(
            self.root,
            || true,
            || Ok(None),
            |ticket, plan| ticket.persist_for_watch_test(self.root, plan, true, fault),
        )
    }

    /// The backend branches startup uses after capture: resume a confirmed admission, or
    /// admit the readable staged offer. Never executes a setup stage.
    pub fn recover(&self) -> Result<u64, String> {
        let mut ticket = InitialSetupAuthorization::capture_at(self.root)?;
        if let Some(admitted) = admission_at(self.root)? {
            if admitted.completed {
                return Err("the recovered setup has no pending stages".into());
            }
            return Ok(admitted.revision);
        }
        let plan = ticket.offer_reading(self.root, || Ok(None))?;
        ticket
            .persist_for_watch_test(self.root, &plan, false, None)?
            .ok_or_else(|| "the recovered startup admission was refused".into())
    }

    pub fn snapshot(&self) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
        snapshot_at(self.root, Plan::default())
    }

    /// Leave the admitted Permissions stage parked instead of completed.
    pub fn leave_permissions_pending(&self) -> Result<(), String> {
        let mut pending = progress(self.root)?.ok_or("the provisional run is missing")?;
        pending.completed = false;
        pending.done &= !Stage::Permissions.bit();
        save_progress(self.root, pending)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.stop_revision(revision_at(self.root)?)
    }

    /// Publish a delayed Stop for an earlier scratch run through the real writer.
    pub fn stop_revision(&self, revision: u64) -> Result<(), String> {
        request_stop_at(self.root, revision)
    }

    pub fn choose(&self) -> Result<(), String> {
        let _lock = HeldLock::take(self.root)?;
        InitialSetupRestart::chosen()
            .persist_with(
                self.root,
                &scoped(Plan::default(), false),
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )?
            .ok_or("the person's replacement was refused")?;
        Ok(())
    }

    /// An actual Set up now of this same plan, preserving its provisional origin.
    pub fn resume(&self) -> Result<u64, String> {
        let _lock = HeldLock::take(self.root)?;
        let plan = saved_plan(self.root)?.ok_or("the saved plan is missing")?;
        InitialSetupRestart::default()
            .changing(Some(Selection::none()))
            .persist_with(
                self.root,
                &plan,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )?
            .ok_or_else(|| "the resumed plan was refused".into())
    }
}
