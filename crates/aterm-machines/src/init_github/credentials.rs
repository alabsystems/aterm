// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's converge pass: the keychain block an earlier `aterm init` wrote to git's global
//! settings gives way to aterm's own helper, read from those settings
//! with its revert journaled by the engine; then whether aterm's route hands back the vault's token. This
//! path inventories no repositories, asks GitHub nothing and stores no credential. A section of
//! GitHub CLI's helper gives way only where the token is here ([`super::token_here`]).

use super::{
    Asked, Github, KEYCHAIN_BLOCK, TOKEN_FILE, asked, helper, profile, read_token, token_here,
};
use crate::git_credential::{Git, Helper, Lookups};
use crate::model::User;
use crate::port::System;
use crate::sync::GithubRead;
use std::path::Path;

/// The account this pass reads for, and whether init.toml's `[github]` names a custom token
/// source. Its settings are this login's alone, and `[github] enabled = false` leaves them to
/// the person.
fn prepare(sys: &dyn System) -> Result<(User, bool), GithubRead> {
    let user = sys.identity().map_err(GithubRead::Deferred)?.user;
    if user.uid != sys.euid() || user.uid == 0 {
        return Err(GithubRead::NotApplicable(
            "GitHub credentials belong to this login alone".into(),
        ));
    }
    let profile = profile(sys, &user)
        .map_err(GithubRead::Deferred)?
        .unwrap_or_default();
    let custom = match asked(profile.github, &user).map_err(GithubRead::Deferred)? {
        Asked::Off => {
            return Err(GithubRead::NotApplicable(
                "off: [github] enabled = false".into(),
            ));
        }
        Asked::Profile(config) => {
            config.token_file != Path::new(&user.home).join(TOKEN_FILE)
                || config.token_key.is_some()
        }
        Asked::Scoped(_) | Asked::Default => false,
    };
    if !Git::for_user(sys, &user).installed(sys) {
        return Err(GithubRead::Deferred(super::NEEDS_GIT.into()));
    }
    Ok((user, custom))
}

/// Read whether the keychain block an earlier `aterm init` wrote is there to replace (from git's
/// settings, and for a section of GitHub CLI's helper the token's presence), and otherwise
/// whether aterm's route hands back the vault's token, without listing or cloning repositories or
/// asking GitHub. An explicitly disabled stage is left unchanged, and a custom token source's
/// lookups are its own item's to read.
///
/// # Errors
/// git's settings or its credential reply cannot be read completely.
pub fn vault_git_credentials(sys: &dyn System) -> Result<GithubRead, String> {
    let (user, custom) = match prepare(sys) {
        Ok(read) => read,
        Err(status) => return Ok(status),
    };
    let git = Git::for_user(sys, &user);
    if let Some(block) = git.keychain_block(sys, &user, &|| token_here(sys, &user))? {
        return Ok(if helper(sys, &user).is_some() {
            GithubRead::Due {
                from: KEYCHAIN_BLOCK.into(),
                revert: block.revert().to_owned(),
            }
        } else {
            GithubRead::Deferred(format!(
                "{KEYCHAIN_BLOCK}; {}",
                Helper::unavailable(sys, &user)
            ))
        });
    }
    if custom {
        return Ok(GithubRead::NotApplicable(
            "a custom GitHub token source is left unchanged".into(),
        ));
    }
    let source = Github {
        account: String::new(),
        owner: String::new(),
        token_file: Path::new(&user.home).join(TOKEN_FILE),
        token_key: None,
    };
    let token = match read_token(sys, &user, &source) {
        Ok(token) => token,
        Err(unread) => return Ok(GithubRead::Deferred(unread.why)),
    };
    let helper = Helper::vault(sys, &user);
    Ok(match git.lookups(sys, &user, helper.as_ref(), &token)? {
        Lookups::Ready => GithubRead::Current("vault GitHub development lookup verified".into()),
        Lookups::Not(why) => {
            GithubRead::NotApplicable(format!("no keychain block to replace; {why}"))
        }
    })
}

/// Replace the keychain block an earlier `aterm init` wrote to git's global settings with aterm's
/// own helper, and nothing else, when it is still the block whose `journaled` revert the engine
/// recorded. The engine's read-back says how the route stands then.
///
/// # Errors
/// This login, init.toml or git no longer admit the pass; git's settings cannot be read whole,
/// or hold a different block than `journaled` puts back; the launcher is gone; or a `git config`
/// run failed (output withheld), when the files may hold part of the change, which `journaled`
/// puts back.
pub fn sync_vault_git_credentials(sys: &dyn System, journaled: &str) -> Result<String, String> {
    let (user, _) =
        prepare(sys).map_err(|status| format!("GitHub settings changed: {status:?}"))?;
    let git = Git::for_user(sys, &user);
    let Some(block) = git.keychain_block(sys, &user, &|| token_here(sys, &user))? else {
        return Ok("no keychain GitHub block is left to replace".into());
    };
    if block.revert() != journaled {
        return Err("git's GitHub settings changed since their revert was journaled".into());
    }
    let helper = helper(sys, &user).ok_or_else(|| Helper::unavailable(sys, &user))?;
    git.replace_keychain_block(sys, &user, &block, &helper, &|| {
        let (current, _) =
            prepare(sys).map_err(|status| format!("GitHub settings changed: {status:?}"))?;
        if current != user || !git.installed(sys) {
            return Err("GitHub login or Git changed during replacement".into());
        }
        if super::helper(sys, &user).as_ref() != Some(&helper) {
            return Err("GitHub credential helper changed during replacement".into());
        }
        Ok(())
    })?;
    Ok("replaced the keychain GitHub block with aterm's helper; no repositories changed".into())
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
