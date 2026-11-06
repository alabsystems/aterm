// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keychain block an earlier `aterm init` wrote to git's global settings gives way to aterm's
//! own route (owner ruling 2026-11-03), in every GitHub mode but `[github] enabled = false`, whichever
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
                        "{KEYCHAIN_BLOCK}, and aterm's helper runs {}, which is not there \
                         (atpkg repair puts it back); removing the block alone would leave \
                         git's own login-keychain helper in effect",
                        Helper::launcher(&self.user).display()
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
        let git = Git::for_user(sys, &self.user);
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
        let helper = super::helper(sys, &self.user).ok_or_else(|| {
            format!(
                "{} left before the keychain GitHub block could be replaced",
                Helper::launcher(&self.user).display()
            )
        })?;
        git.replace_keychain_block(sys, &self.user, &block, &helper)?;
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
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::git_credential::script;
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    /// A Mac with git, a launcher and no token file: the item needs nothing more.
    fn mac() -> (Fake, User, Ctx) {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.file(crate::items::github::CLT_GIT, b"git");
        let who = user("alice", 501);
        script::launcher(&sys, &who);
        let context = ctx(std::slice::from_ref(&who));
        (sys, who, context)
    }

    /// No helper ran, and neither did anything that reaches a token or GitHub.
    fn settings_alone(sys: &Fake) {
        let calls = sys.calls();
        assert!(
            calls.iter().all(|call| !call.contains("credential fill")
                && !call.contains("agh ")
                && !call.contains("osxkeychain get")),
            "{calls:?}"
        );
    }

    /// The block an earlier init wrote gives way with no token on the machine and no GitHub to
    /// ask: the plan reads the settings alone and names its revert; the apply replaces the block
    /// with aterm's helper for both hosts, and the read-back holds.
    #[test]
    fn the_block_gives_way_from_the_settings_alone() {
        let (sys, who, context) = mac();
        script::keychain_block(&sys, &who, "alice");
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Plan, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(rows[0].from.as_deref(), Some(KEYCHAIN_BLOCK));
        let revert = rows[0].revert.as_deref().unwrap_or_default();
        assert!(
            revert.contains("--add credential.https://github.com.helper osxkeychain")
                && revert.contains("--add credential.https://github.com.username alice"),
            "{revert}"
        );
        assert_eq!(sys.calls(), [script::helpers(&who)]);

        let (sys, who, context) = mac();
        script::keychain_block(&sys, &who, "alice");
        script::keychain_block(&sys, &who, "alice");
        script::alike(&sys, &who);
        let writes = script::replaces_keychain_block(&sys, &who);
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert!(
            writes.iter().all(|write| calls.contains(write)),
            "{calls:?}"
        );
        settings_alone(&sys);
    }

    /// `[github] enabled = false` leaves git's settings to the person, whichever entry point runs
    /// the row, and it reads them no further. An init.toml that cannot be admitted says nothing
    /// about the switch, so the row cannot read and writes nothing: never the vault's route in
    /// its place.
    #[test]
    fn the_off_switch_and_an_init_toml_it_cannot_admit_write_nothing() {
        for (text, want) in [
            ("[github]\nenabled = false\n", Outcome::NotApplicable),
            ("[github]\nowner = \"alice\"\n", Outcome::Unknown),
            ("not toml at all", Outcome::Unknown),
        ] {
            let (sys, who, context) = mac();
            script::keychain_block(&sys, &who, "alice");
            let file = format!("{}/.config/aterm/init.toml", who.home);
            sys.file(&file, text.as_bytes()).stat_of(
                &file,
                crate::port::Stat {
                    uid: who.uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: text.len() as u64,
                },
            );
            let items: [Box<dyn Item>; 1] = [item(&who)];
            let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
            assert_eq!(rows[0].outcome, want, "{text}: {rows:?}");
            let calls = sys.calls();
            assert!(!calls.contains(&script::helpers(&who)), "{calls:?}");
            assert!(
                calls
                    .iter()
                    .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
                "{calls:?}"
            );
        }
    }

    /// The vault's token, as the helper would read it.
    fn token(sys: &Fake, who: &User) {
        let file = format!("{}/.secrets/gh_access_token", who.home);
        sys.file(&file, b"fixture_token_only").stat_of(
            &file,
            crate::port::Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 18,
            },
        );
    }

    /// `gh auth setup-git` writes this section for anyone, so with no token for aterm's helper to
    /// hand back it stays: a helper with none would end the person's `git push`. The row writes
    /// nothing and names it.
    #[test]
    fn the_setup_git_block_stays_where_atermss_helper_has_no_token() {
        let (sys, who, context) = mac();
        script::gh_setup_git_block(&sys, &who);
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        let why = rows[0].why.as_deref().unwrap_or_default();
        assert!(
            why.contains("may still reach the login keychain")
                && why.contains("credential.https://github.com.helper"),
            "{why}"
        );
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
            "{calls:?}"
        );
        settings_alone(&sys);
    }

    /// A conditional include git refuses to read only colours the row's note: the row stands, so
    /// no retry and no stage waits on a file git reads in some other repository.
    #[test]
    fn an_include_git_refuses_leaves_the_row_standing() {
        let (sys, who, context) = mac();
        let global = format!("{}/.gitconfig", who.home);
        let work = format!("{}/work.inc", who.home);
        sys.on(
            &script::helpers(&who),
            Ran::ok(&script::printed(&[(
                global.as_str(),
                "includeif.gitdir:~/work/.path",
                "~/work.inc",
            )])),
        );
        sys.on(
            &script::included(&who, &work),
            Ran::exit(128, "fatal: bad config line 1"),
        );
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::NotApplicable, "{rows:?}");
        assert!(
            rows[0].why.as_deref().unwrap_or_default().contains(&work),
            "{rows:?}"
        );
    }

    /// What `gh auth setup-git` wrote goes the same way, where the token is here.
    #[test]
    fn the_setup_git_block_gives_way_too() {
        let (sys, who, context) = mac();
        token(&sys, &who);
        script::gh_setup_git_block(&sys, &who);
        script::gh_setup_git_block(&sys, &who);
        script::alike(&sys, &who);
        let writes = script::replaces_gh_setup_git_block(&sys, &who);
        let items: [Box<dyn Item>; 1] = [item(&who)];
        let rows = engine::run(&items, Mode::Apply, &sys, &context, &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert!(
            writes.iter().all(|write| calls.contains(write)),
            "{calls:?}"
        );
        settings_alone(&sys);
    }

    /// Removing the block with no helper to put in its place would turn the system's keychain
    /// helper back on: with no launcher the row waits for one and writes nothing.
    #[test]
    fn no_launcher_leaves_the_block_where_it_is() {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.file(crate::items::github::CLT_GIT, b"git");
        let who = user("alice", 501);
        let context = ctx(std::slice::from_ref(&who));
        script::keychain_block(&sys, &who, "alice");
        let keychain = Keychain::new(who.clone());
        let Probe::Blocked { why } = keychain.probe(&sys, &context) else {
            panic!("no launcher, no replacement");
        };
        assert!(why.contains("/Users//alice/.local/bin/aterm"), "{why}");
        assert_eq!(keychain.revert(), None);
        assert_eq!(sys.calls(), [script::helpers(&who)]);
    }

    /// A block that changed after the probe journaled its revert is left: that revert would not
    /// put back what is there.
    #[test]
    fn a_block_that_changed_after_the_probe_is_left() {
        let (sys, who, context) = mac();
        script::keychain_block(&sys, &who, "alice");
        script::keychain_block(&sys, &who, "someone-else");
        let keychain = Keychain::new(who.clone());
        assert!(matches!(
            keychain.probe(&sys, &context),
            Probe::Change { .. }
        ));
        assert!(keychain.apply(&sys, &context).is_err());
        assert!(
            sys.calls().iter().all(|call| !call.contains("--unset-all")),
            "{:?}",
            sys.calls()
        );
    }

    /// Nothing of an earlier init's is no step of this run, and a machine with no git has
    /// nothing to read: neither row says the stage is done or asks for anything. A keychain
    /// helper of someone else's stays and is named; aterm's own git never asks it.
    #[test]
    fn settings_of_someone_elses_are_named_and_left() {
        let (sys, who, context) = mac();
        script::alike(&sys, &who);
        let keychain = Keychain::new(who.clone());
        assert!(matches!(
            keychain.probe(&sys, &context),
            Probe::NotApplicable { .. }
        ));
        settings_alone(&sys);

        let bare = Fake::root();
        assert!(matches!(
            Keychain::new(who.clone()).probe(&bare, &context),
            Probe::NotApplicable { .. }
        ));
        assert!(bare.calls().is_empty(), "{:?}", bare.calls());

        let (sys, who, context) = mac();
        let global = "/Users//alice/.gitconfig";
        let helper = "credential.https://github.com.helper";
        sys.on(
            &script::helpers(&who),
            Ran::ok(&script::printed(&[
                (global, helper, ""),
                (global, helper, "osxkeychain"),
                (global, helper, "!other-stand-in"),
            ])),
        );
        let keychain = Keychain::new(who.clone());
        let Probe::NotApplicable { why } = keychain.probe(&sys, &context) else {
            panic!("someone else's section is no block");
        };
        assert!(why.contains(helper) && why.contains(global), "{why}");
        assert_eq!(keychain.revert(), None);
        settings_alone(&sys);
        assert!(
            sys.calls().iter().all(|call| !call.contains("--add")),
            "{:?}",
            sys.calls()
        );
    }
}
