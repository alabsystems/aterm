// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Repository intent from an admitted inventory, before any credential or Git use.

use super::native::Block;
use super::{Asked, VAULT_INPUT_ENTRIES, asked, inventory, profile, repos};
use crate::init_observe::Wait;
use crate::items::Ctx;
use crate::model::User;
use crate::port::System;
use aterm_secrets::{client, need};
use std::path::Path;

/// The ordinary credential protocol owns delivery; this is no GitHub authentication claim.
const CREDENTIAL_WAIT: &str =
    "selected repositories are waiting for the vault's development credential";

/// A current default inventory's desired work. This grants no credential, helper or Git authority.
#[derive(Clone)]
pub struct RepositorySelection {
    user: User,
    paths: client::Paths,
    config: client::Config,
    snapshot: client::MaterializationSnapshot,
    count: usize,
}

impl RepositorySelection {
    /// Read the default inventory under its ready source, independently of credential delivery.
    /// Explicit profiles and an unconfigured vault retain their existing local policy.
    /// # Errors
    /// The account, source, complete materialization or inventory is unsafe or unavailable.
    pub fn capture(sys: &dyn System, ctx: &Ctx) -> Result<Option<Self>, String> {
        let [user] = ctx.users.as_slice() else {
            return Err("repository selection requires exactly one local user".into());
        };
        if user.uid == 0 || sys.euid() != user.uid {
            return Err("repository selection belongs to its local account".into());
        }
        Self::for_user(sys, user).map_err(|block| block.why)
    }

    fn configured_source(
        sys: &dyn System,
        user: &User,
    ) -> Result<Option<(client::Paths, client::Config)>, Block> {
        if user.uid == 0 || sys.euid() != user.uid {
            return Ok(None);
        }
        let profile = profile(sys, user).map_err(Block::of(Wait::Settings))?;
        if !matches!(
            asked(profile.and_then(|p| p.github), user).map_err(Block::of(Wait::Settings))?,
            Asked::Default
        ) {
            return Ok(None);
        }
        let paths = client::Paths::for_home(Path::new(&user.home));
        let Some(config) = client::config(&paths).map_err(|why| Block::from(why.to_string()))?
        else {
            return Ok(None);
        };
        repos::admit_destination(sys, user)?;
        if sys.identity()?.user != *user {
            return Err(Block::from(
                "the repository selection account changed".to_owned(),
            ));
        }
        Ok(Some((paths, config)))
    }

    pub(super) fn for_user(sys: &dyn System, user: &User) -> Result<Option<Self>, Block> {
        let Some((paths, config)) = Self::configured_source(sys, user)? else {
            return Ok(None);
        };
        let Some(ready) = client::with_materialization_ready_for_entries(
            &paths,
            &config,
            &[inventory::ENTRY],
            || Ok(inventory::Inventory::read(sys, user)),
        )
        .map_err(|why| Block::from(why.to_string()))?
        else {
            return Err(Block::from(
                "the configured vault is not ready for the private repository inventory".to_owned(),
            ));
        };
        let inventory = ready.inputs.map_err(Block::of(Wait::RepositoryInventory))?;
        let selection = Self {
            user: user.clone(),
            paths,
            config,
            snapshot: ready.snapshot,
            count: inventory.repositories().len(),
        };
        selection.ensure_current(sys)?;
        Ok(Some(selection))
    }

    /// The complete validated selection, not a count of acquired checkouts.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Desired work while delivery is pending, without inventing a physical checkout count.
    #[must_use]
    pub fn waiting(&self) -> String {
        format!("{} repositories selected; {CREDENTIAL_WAIT}", self.count)
    }

    /// Reobserve account, default intent and the ready inventory source before using this reading.
    /// # Errors
    /// The captured selection no longer holds or the current source cannot be read.
    pub fn ensure_current(&self, sys: &dyn System) -> Result<(), String> {
        if sys.stopped().is_some()
            || sys.euid() != self.user.uid
            || sys.identity()?.user != self.user
            || !matches!(
                asked(profile(sys, &self.user)?.and_then(|p| p.github), &self.user)?,
                Asked::Default
            )
            || !self
                .snapshot
                .is_current_now(&self.paths)
                .map_err(|why| why.to_string())?
        {
            return Err("repository selection changed, stopped or is still syncing".into());
        }
        Ok(())
    }

    /// Whether complete inventory materialization still lacks the registered credential.
    /// No credential bytes, helper or Git program are read here.
    /// # Errors
    /// The source is no longer a complete current selection.
    pub fn needs_credential(&self, sys: &dyn System) -> Result<bool, String> {
        self.ensure_current(sys)?;
        if self.count == 0 {
            return Ok(false);
        }
        let ready = client::with_materialization_ready_for_entries(
            &self.paths,
            &self.config,
            &VAULT_INPUT_ENTRIES,
            || Ok(()),
        )
        .map_err(|why| why.to_string())?
        .is_some();
        self.ensure_current(sys)?;
        Ok(!ready)
    }

    /// Record one durable, deduplicated want after releasing every materialization lock.
    /// The existing need policy and credential protocol retain all release authority.
    /// # Errors
    /// Current intent was withdrawn, or the existing need ledger cannot be updated.
    pub fn request_credential(
        &self,
        sys: &dyn System,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<bool, String> {
        current()?;
        if !self.needs_credential(sys)? {
            return Ok(false);
        }
        current()?;
        self.ensure_current(sys)?;
        let want = need::Want::Gh(need::GhProfile::Dev);
        let purpose = need::Purpose::AghDev;
        let previous = need::peek(&self.paths)
            .map_err(|why| why.to_string())?
            .into_iter()
            .find(|need| need.want == want && need.purpose == purpose);
        let delivered = previous
            .filter(|previous| {
                previous.got
                    && matches!(
                        previous.state,
                        need::State::Published | need::State::Fulfilled
                    )
            })
            .and_then(|previous| previous.seq);
        let raised = match delivered {
            Some(seq) => need::raise_refused(&self.paths, want, purpose, seq),
            None => need::raise(&self.paths, want, purpose),
        }
        .map_err(|why| why.to_string())?;
        current()?;
        self.ensure_current(sys)?;
        if raised.state.ended() || raised.state == need::State::AwaitingPerson {
            return Err(format!("{}; {}", self.waiting(), raised.line));
        }
        Ok(true)
    }

    /// The paired source's actual consumer succeeded; retire or fulfill its existing want.
    /// # Errors
    /// Intent changed or the existing ledger cannot be updated.
    pub(super) fn credential_satisfied(
        &self,
        sys: &dyn System,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        if self.count == 0 {
            return Ok(());
        }
        current()?;
        self.ensure_current(sys)?;
        if !need::peek(&self.paths)
            .map_err(|why| why.to_string())?
            .iter()
            .any(|need| {
                need.want == need::Want::Gh(need::GhProfile::Dev)
                    && need.purpose == need::Purpose::AghDev
            })
        {
            return Ok(());
        }
        if self.needs_credential(sys)? {
            return Err(self.waiting());
        }
        current()?;
        need::satisfied(
            &self.paths,
            &need::Want::Gh(need::GhProfile::Dev),
            need::Purpose::AghDev,
        )
        .map_err(|why| why.to_string())?;
        current()?;
        self.ensure_current(sys)
    }

    /// Scheduling only: an actual paired consumer must still accept the credential in Apply.
    pub(super) fn consumer_ack_due(sys: &dyn System, user: &User) -> Result<bool, String> {
        let Some((paths, _)) = Self::configured_source(sys, user).map_err(|block| block.why)?
        else {
            return Ok(false);
        };
        need::peek(&paths)
            .map(|needs| {
                needs.iter().any(|need| {
                    need.want == need::Want::Gh(need::GhProfile::Dev)
                        && need.purpose == need::Purpose::AghDev
                        && !need.state.ended()
                })
            })
            .map_err(|why| why.to_string())
    }

    /// Read-only scheduling for a delivered credential whose actual consumer still owes an ack.
    /// # Errors
    /// The account, ledger or selected source cannot be read safely.
    pub fn acknowledgment_due(sys: &dyn System, user: &User) -> Result<bool, String> {
        if !Self::consumer_ack_due(sys, user)? {
            return Ok(false);
        }
        let Some(selection) = Self::for_user(sys, user).map_err(|block| block.why)? else {
            return Ok(false);
        };
        if selection.count == 0 {
            return Ok(false);
        }
        Ok(!selection.needs_credential(sys)?)
    }
}
