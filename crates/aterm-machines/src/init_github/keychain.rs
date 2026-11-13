// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keychain block an earlier `aterm init` wrote to git's global settings gives way to aterm's
//! own route, in every GitHub mode but `[github] enabled = false`, whichever
//! entry point runs it: read from git's settings before any GitHub answer, and replaced so the
//! person's own git asks no keychain helper either
//! ([`crate::git_credential::Git::replace_keychain_block`]). A section of GitHub CLI's helper,
//! which `gh auth setup-git` writes for anyone, gives way only where aterm's helper has a token
//! to hand back ([`super::token_here`]).

use super::KEYCHAIN_BLOCK;
use crate::git_credential::{Git, Helper};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::System;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

/// The change a probe plans.
const REPLACED: &str = "aterm's own GitHub helper in its place, for each host whose section in \
                        that file was the block's or held nothing; the person's other git \
                        settings stay";

/// `aterm init`'s keychain item for `user`.
pub(super) fn item(user: &User) -> Box<dyn Item> {
    Box::new(Keychain::new(user.clone()))
}

/// The item. It waits for nothing and no row waits for it: init's own network git carries its
/// route whether or not the block is still there ([`crate::git_credential::pinned`]).
pub(super) struct Keychain {
    user: User,
    /// The commands that put the block back, read by the probe: the engine journals them before
    /// the apply runs, which replaces that block and no other.
    revert: Mutex<Option<String>>,
    /// The apply replaced the block, so the read-back answers for it.
    replaced: AtomicBool,
}

impl Keychain {
    pub(super) fn new(user: User) -> Self {
        Self {
            user,
            revert: Mutex::new(None),
            replaced: AtomicBool::new(false),
        }
    }
}

impl Item for Keychain {
    fn id(&self) -> String {
        format!("init.github.keychain@{}", self.user.name)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        "git's GitHub credential settings, never through the login keychain".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let mut revert = self.revert.lock().unwrap_or_else(PoisonError::into_inner);
        *revert = None;
        // The switch is read here, so `aterm init`, System State's run and the converge pass
        // agree: an init.toml that cannot be admitted says nothing, and nothing is written on it.
        match super::settings_managed(sys, &self.user) {
            Ok(true) => {}
            Ok(false) => {
                return Probe::NotApplicable {
                    why: "off: [github] enabled = false".into(),
                };
            }
            Err(why) => return Probe::Unusable { why },
        }
        let git = Git::for_user(sys, &self.user);
        if !git.installed(sys) {
            return Probe::NotApplicable {
                why: "no git on this machine reads its settings yet".into(),
            };
        }
        let block =
            match git.keychain_block(sys, &self.user, &|| super::token_here(sys, &self.user)) {
                Ok(block) => block,
                Err(why) => return Probe::Unusable { why },
            };
        if let Some(block) = block {
            if super::helper(sys, &self.user).is_none() {
                return Probe::Blocked {
                    why: format!(
                        "{KEYCHAIN_BLOCK}; {}; removing the block alone would leave \
                         git's own login-keychain helper in effect",
                        Helper::unavailable(sys, &self.user)
                    ),
                };
            }
            *revert = Some(block.revert().to_owned());
            return Probe::Change {
                from: KEYCHAIN_BLOCK.into(),
                to: REPLACED.into(),
            };
        }
        let left = git
            .keychain_in_effect(sys, &self.user)
            .map_or_else(String::new, |helper| {
                format!(
                    "; the person's own git may still reach the login keychain ({helper}); aterm's \
                 own git never asks it"
                )
            });
        if self.replaced.load(Ordering::Relaxed) {
            Probe::Ok {
                now: format!("{REPLACED}{left}"),
            }
        } else {
            Probe::NotApplicable {
                why: format!("nothing in git's global settings for aterm to replace{left}"),
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != self.user.uid {
            return Err("git's GitHub settings belong to their local user".into());
        }
        if !super::settings_managed(sys, &self.user)? {
            return Err("off: [github] enabled = false".into());
        }
        let git = Git::for_user(sys, &self.user);
        if !git.installed(sys) {
            return Err("no executable Git is available to change its settings".into());
        }
        let Some(block) =
            git.keychain_block(sys, &self.user, &|| super::token_here(sys, &self.user))?
        else {
            return Err("the keychain GitHub block changed before it could be replaced".into());
        };
        let journaled = self.revert.lock().unwrap_or_else(PoisonError::into_inner);
        if journaled.as_deref() != Some(block.revert()) {
            return Err("the keychain GitHub block changed before it could be replaced".into());
        }
        drop(journaled);
        let helper =
            super::helper(sys, &self.user).ok_or_else(|| Helper::unavailable(sys, &self.user))?;
        git.replace_keychain_block(sys, &self.user, &block, &helper, &|| {
            if !super::settings_managed(sys, &self.user)? {
                return Err("off: [github] enabled = false".into());
            }
            if !git.installed(sys) {
                return Err("no executable Git is available to change its settings".into());
            }
            if super::helper(sys, &self.user).as_ref() != Some(&helper) {
                return Err("GitHub credential helper changed during replacement".into());
            }
            Ok(())
        })?;
        self.replaced.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        self.revert
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|settings| {
                format!(
                    "{settings} (puts back the GitHub credential block an earlier aterm init \
                     wrote to git's global settings)"
                )
            })
    }
}

#[cfg(test)]
#[path = "keychain_tests.rs"]
mod tests;
