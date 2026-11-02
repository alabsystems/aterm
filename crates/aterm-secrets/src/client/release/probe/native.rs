// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native effects. Every child rechecks the current member and credential snapshot.

use super::{
    apple::{Notary, Tool, ToolError},
    artifact, credentials,
    engine::{Finality, Host, Outcome, Receipt},
    model::{Action, Journal},
    port::Invocation,
    remote::{Api, Remote},
    scope::Scope,
};
use crate::{Error, Result};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) struct Native {
    pub scope: Scope,
    pub dir: PathBuf,
    fresh_uploads: RefCell<BTreeSet<usize>>,
}

enum AppleAction {
    Submit,
    Recover,
    Check,
}

struct Tools<'a> {
    scope: &'a Scope,
    journal: &'a Journal,
    profile: Option<&'a str>,
    identity: Option<&'a str>,
    developer: Option<&'a Path>,
    deadline: Instant,
}

impl Tool for Tools<'_> {
    fn run(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        deadline: Instant,
    ) -> std::result::Result<std::process::Output, ToolError> {
        let deadline = deadline.min(self.deadline);
        if Instant::now() >= deadline {
            return Err(ToolError::refused(command, "deadline-before-start"));
        }
        self.scope
            .require(&self.journal.subject, deadline)
            .map_err(|_| ToolError::refused(command, "admission"))?;
        if self.profile.is_some_and(|profile| {
            !credentials::notary_profile(self.scope, self.journal, deadline)
                .is_ok_and(|current| current == profile)
        }) {
            return Err(ToolError::refused(command, "profile-changed"));
        }
        if self.identity.is_some_and(|identity| {
            !super::super::signing::probe_identity(&self.scope.paths)
                .is_ok_and(|current| current == identity)
        }) {
            return Err(ToolError::refused(command, "identity-changed"));
        }
        if let Some(developer) = self.developer {
            if (super::super::LIVE_DEVELOPER_DIR)().as_deref() != Some(developer)
                || command.get_program() != developer.join("usr/bin/notarytool").as_os_str()
            {
                return Err(ToolError::refused(command, "developer-tool-changed"));
            }
            command.env("DEVELOPER_DIR", developer);
        }
        if Instant::now() >= deadline {
            return Err(ToolError::refused(command, "deadline-before-start"));
        }
        let started = std::cell::Cell::new(false);
        aterm_uds::deadline::output_until_complete_stdout_observed(
            command,
            deadline.min(Instant::now() + Duration::from_secs(60)),
            || started.set(true),
        )
        .map_err(|error| {
            let error = ToolError::observed(command, &error, started.get());
            eprintln!("{error}");
            error
        })
    }
}

impl crate::keychain_tools::Run for Tools<'_> {
    fn run(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
        wait: Duration,
    ) -> std::result::Result<std::process::Output, aterm_uds::deadline::RunError> {
        Tool::run(
            self,
            command,
            Instant::now() + wait.min(Duration::from_secs(60)),
        )
        .map_err(|_| {
            aterm_uds::deadline::RunError::Wait(std::io::Error::other(
                "probe signing authorization or tool failed",
            ))
        })
    }
}

impl Native {
    pub(super) fn new(scope: Scope, dir: PathBuf) -> Self {
        Self {
            scope,
            dir,
            fresh_uploads: RefCell::new(BTreeSet::new()),
        }
    }

    fn checked_readback(
        &self,
        remote: &Remote<'_>,
        journal: &Journal,
        index: usize,
    ) -> Result<Outcome> {
        let outcome = self.fetch_readback(remote)?;
        self.remember_readback(journal, index, &outcome)?;
        Ok(outcome)
    }

    fn fetch_readback(&self, remote: &Remote<'_>) -> Result<Outcome> {
        let temp =
            aterm_tempfile::tempdir().map_err(|_| Error::new("cannot stage probe readback"))?;
        let target = temp.path().join("asset.zip");
        crate::fsx::write_new(&target, b"", 0o600)?;
        remote.readback(&target)
    }

    fn remember_readback(&self, journal: &Journal, index: usize, outcome: &Outcome) -> Result<()> {
        if matches!(outcome, Outcome::Applied(Receipt::Readback {sha256, bytes})
            if journal.artifact.as_ref().is_some_and(|artifact| artifact.sha256 == *sha256 && artifact.bytes == *bytes))
        {
            self.fresh_uploads.borrow_mut().insert(index);
            Ok(())
        } else {
            Err(Error::new(
                "probe readback no longer matches the signed artifact",
            ))
        }
    }

    fn cleanup_with(
        &self,
        journal: &Journal,
        mut read: impl FnMut(usize) -> Result<Option<Outcome>>,
        effect: impl FnOnce() -> Result<Outcome>,
    ) -> Result<Outcome> {
        for (index, repo) in journal.repositories.iter().enumerate() {
            if repo.asset.is_none() || self.fresh_uploads.borrow().contains(&index) {
                continue;
            }
            if let Some(outcome) = read(index)? {
                self.remember_readback(journal, index, &outcome)?;
            }
        }
        effect()
    }
    fn tools<'a>(&'a self, journal: &'a Journal, deadline: Instant) -> Tools<'a> {
        Tools {
            scope: &self.scope,
            journal,
            profile: None,
            identity: None,
            developer: None,
            deadline,
        }
    }

    fn sign(&self, journal: &Journal, deadline: Instant) -> Result<Outcome> {
        let identity = super::super::signing::probe_identity(&self.scope.paths)?;
        let _lease =
            crate::release_keychain::signing_lease(&self.scope.paths.home).map_err(Error::new)?;
        let tools = Tools {
            identity: Some(&identity),
            ..self.tools(journal, deadline)
        };
        let keychains = crate::release_keychain::Keychains::new(&self.scope.paths.home, &tools);
        let keychain = keychains
            .signing_keychain(
                aterm_update_core::pins::APPLE_TEAM_ID,
                &identity,
                crate::release_keychain::Leave::Write,
            )
            .map_err(|_| {
                Error::new("probe signing keychain is not ready; normal convergence will repair it")
            })?
            .ok_or_else(|| Error::new("probe requires its owned private signing keychain"))?;
        let artifact =
            artifact::prepare(&tools, &self.dir, journal, &identity, &keychain, deadline)?;
        self.scope.require(&journal.subject, deadline)?;
        super::super::signing::record_probe_signature(&self.scope.paths, &identity)?;
        Ok(Outcome::Applied(Receipt::Signed(artifact)))
    }

    fn apple(&self, journal: &Journal, deadline: Instant, action: AppleAction) -> Result<Outcome> {
        let prepared = (|| {
            let developer = (super::super::LIVE_DEVELOPER_DIR)()
                .ok_or_else(|| Error::new("probe awaits installed notarization tools"))?;
            artifact::verify(&self.tools(journal, deadline), &self.dir, journal, deadline)?;
            let profile = credentials::notary_profile(&self.scope, journal, deadline)?;
            Ok::<_, Error>((developer, profile))
        })();
        let (developer, profile) = match prepared {
            Err(_) if matches!(action, AppleAction::Submit) => {
                return Ok(Outcome::NotStarted(
                    "Apple submission preflight is not ready",
                ));
            }
            other => other?,
        };
        let tools = Tools {
            profile: Some(&profile),
            developer: Some(&developer),
            ..self.tools(journal, deadline)
        };
        let archive = artifact::archive(&self.dir, &journal.nonce);
        let program = developer.join("usr/bin/notarytool");
        let notary = Notary {
            tool: &tools,
            program: &program,
            profile: &profile,
            archive: &archive,
            journal,
        };
        match action {
            AppleAction::Submit => notary.submit(deadline),
            AppleAction::Recover => notary.observe_submission(deadline),
            AppleAction::Check => notary.accepted(deadline),
        }
    }

    fn github(
        &self,
        journal: &Journal,
        action: Action,
        observe: bool,
        deadline: Instant,
    ) -> Result<Outcome> {
        if matches!(
            action,
            Action::DeleteAsset(_) | Action::DeleteDraft(_) | Action::DeleteTag(_)
        ) {
            return self.cleanup_with(
                journal,
                |index| {
                    let route = credentials::route(&self.scope, journal, index, deadline)?;
                    let remote = Remote {
                        api: &route.http,
                        journal,
                        index,
                        principal: &route.principal,
                    };
                    if remote.has_owned_asset()? {
                        self.fetch_readback(&remote).map(Some)
                    } else {
                        Ok(None)
                    }
                },
                || self.github_effect(journal, action, observe, deadline),
            );
        }
        self.github_effect(journal, action, observe, deadline)
    }

    fn github_effect(
        &self,
        journal: &Journal,
        action: Action,
        observe: bool,
        deadline: Instant,
    ) -> Result<Outcome> {
        let index = action
            .index()
            .ok_or_else(|| Error::new("probe Git action has no repository"))?;
        let route = match credentials::route(&self.scope, journal, index, deadline) {
            Ok(route) => route,
            Err(error) if observe => return Err(error),
            Err(error) => {
                super::diagnostic("github-credential-route", &error);
                return Ok(Outcome::NotStarted(
                    "probe credential preflight is unavailable",
                ));
            }
        };
        let repo = &journal.repositories[index];
        let scratch = match self.git_dir(index) {
            Ok(scratch) => scratch,
            Err(error) if observe => return Err(error),
            Err(error) => {
                super::diagnostic("github-scratch", &error);
                return Ok(Outcome::NotStarted(
                    "probe scratch preflight is unavailable",
                ));
            }
        };
        let invocation = Invocation {
            owner: &repo.owner,
            principal: &route.principal,
            scratch: &scratch,
            deadline,
            current: &*route.current,
        };
        let git = &self.scope.git;
        let remote = Remote {
            api: &route.http,
            journal,
            index,
            principal: &route.principal,
        };
        match action {
            Action::Fetch(_) => Ok(Outcome::Applied(Receipt::Fetched {
                commit: git.fetch_head(&invocation)?,
                principal: route.principal.clone(),
            })),
            Action::Tag(_) => {
                let oid = repo.commit.as_deref().unwrap();
                let current = match git.tag(&invocation, &journal.tag()) {
                    Ok(current) => current,
                    Err(error) if observe => return Err(error),
                    Err(error) => {
                        super::diagnostic("github-tag-read", &error);
                        return Ok(Outcome::NotStarted("probe tag preflight is unavailable"));
                    }
                };
                if observe {
                    return match current {
                        Some(found) if found == oid => Ok(Outcome::Applied(Receipt::Tag)),
                        Some(_) => Err(Error::new("probe tag names a foreign commit")),
                        None => Ok(Outcome::Pending("tag creation remains unconfirmed")),
                    };
                }
                if current.is_some() {
                    return Ok(Outcome::NotStarted("probe tag already exists"));
                }
                match git.create_tag(&invocation, &journal.tag(), oid) {
                    Ok(()) => Ok(Outcome::Applied(Receipt::Tag)),
                    Err(super::port::EffectError::NotStarted) => {
                        Ok(Outcome::NotStarted("tag creation did not start"))
                    }
                    Err(_) => Ok(Outcome::Pending("tag creation needs observation")),
                }
            }
            Action::Draft(_) if observe => remote.observe_draft(),
            Action::Draft(_) => remote.create_draft(),
            Action::Upload(_) if observe => remote.observe_upload(),
            Action::Upload(_) => {
                if let Err(error) =
                    artifact::verify(&self.tools(journal, deadline), &self.dir, journal, deadline)
                {
                    super::diagnostic("github-upload-artifact", &error);
                    return Ok(Outcome::NotStarted(
                        "probe upload artifact preflight is unavailable",
                    ));
                }
                remote.upload(&artifact::archive(&self.dir, &journal.nonce))
            }
            Action::Readback(_) => self.checked_readback(&remote, journal, index),
            Action::DeleteAsset(_) => remote.delete_asset(observe),
            Action::DeleteDraft(_) => remote.delete_draft(observe),
            Action::DeleteTag(_) => {
                // A tag belongs to this probe only while no release now claims its name.
                remote.require_no_release()?;
                match git.tag(&invocation, &journal.tag())? {
                    None => Ok(Outcome::Applied(Receipt::TagAbsent)),
                    Some(oid) if Some(oid.as_str()) == repo.commit.as_deref() => {
                        // Deleting this exact leased ref is idempotent; after a lost reply,
                        // a fresh same-OID observation permits this same guarded deletion.
                        match git.delete_tag(&invocation, &journal.tag(), &oid) {
                            Ok(()) => Ok(Outcome::Applied(Receipt::TagAbsent)),
                            Err(_) => Ok(Outcome::Pending("tag deletion needs observation")),
                        }
                    }
                    Some(_) => Err(Error::new("probe refuses to delete a changed tag")),
                }
            }
            _ => Err(Error::new("unexpected native GitHub probe action")),
        }
    }

    fn git_dir(&self, index: usize) -> Result<PathBuf> {
        let dir = self.dir.join(format!("repository-{index}.git"));
        if matches!(crate::fsx::kind(&dir)?, crate::fsx::Kind::Missing) {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|_| Error::new("cannot reserve probe Git directory"))?;
            aterm_durable::durable_dir(&self.dir)?;
        }
        aterm_uds::private_file::check_private_dir(&dir)?;
        Ok(dir)
    }
}

impl Host for Native {
    fn authorize(&mut self, journal: &Journal, deadline: Instant) -> Result<()> {
        self.scope.require(&journal.subject, deadline).map(|_| ())
    }

    fn perform(&mut self, journal: &Journal, action: Action, deadline: Instant) -> Result<Outcome> {
        match action {
            Action::Sign => self.sign(journal, deadline),
            Action::Submit => self.apple(journal, deadline, AppleAction::Submit),
            _ => self.github(journal, action, false, deadline),
        }
    }

    fn observe(&mut self, journal: &Journal, action: Action, deadline: Instant) -> Result<Outcome> {
        match action {
            Action::Sign if journal.unsigned_cleanup() => {
                if artifact::unsigned_retained(&self.dir, journal)? {
                    Ok(Outcome::Applied(Receipt::UnsignedRetained))
                } else {
                    Err(Error::new(
                        "pending signing has no safe unsigned cleanup observation",
                    ))
                }
            }
            Action::Sign => self.sign(journal, deadline),
            Action::Submit => self.apple(journal, deadline, AppleAction::Recover),
            _ => self.github(journal, action, true, deadline),
        }
    }

    fn notary(&mut self, journal: &Journal, deadline: Instant) -> Result<Outcome> {
        self.apple(journal, deadline, AppleAction::Check)
    }

    fn final_check(
        &mut self,
        journal: &Journal,
        full: bool,
        deadline: Instant,
    ) -> Result<Finality> {
        self.scope.require(&journal.subject, deadline)?;
        if full {
            if !matches!(
                super::super::signing::signing_status(&self.scope.paths)?,
                super::super::signing::SigningStatus::Ready(_)
            ) {
                return Err(Error::new(
                    "probe awaits a current unattended signing proof",
                ));
            }
            if journal
                .artifact
                .as_ref()
                .map(|a| a.signing_identity.as_str())
                != Some(super::super::signing::probe_identity(&self.scope.paths)?.as_str())
            {
                return Err(Error::new(
                    "probe no longer uses the current authorized signing material",
                ));
            }
            if !matches!(
                self.apple(journal, deadline, AppleAction::Check)?,
                Outcome::Applied(Receipt::Accepted { .. })
            ) {
                return Err(Error::new(
                    "probe Apple acceptance is not currently confirmed",
                ));
            }
        }
        for (index, repo) in journal.repositories.iter().enumerate() {
            if !repo.tag_owned && repo.release.is_none() && repo.asset.is_none() {
                continue;
            }
            let route = credentials::route(&self.scope, journal, index, deadline)?;
            Remote {
                api: &route.http,
                journal,
                index,
                principal: &route.principal,
            }
            .require_no_release()?;
            for path in [
                repo.asset
                    .map(|id| format!("/repos/{}/aterm/releases/assets/{id}", repo.owner)),
                repo.release
                    .map(|id| format!("/repos/{}/aterm/releases/{id}", repo.owner)),
            ]
            .into_iter()
            .flatten()
            {
                if route
                    .http
                    .request(aterm_update_core::github_rest::Method::Get, &path, None)?
                    .status
                    != 404
                {
                    return Err(Error::new(
                        "probe remote cleanup is not currently confirmed",
                    ));
                }
            }
            let scratch = self.git_dir(index)?;
            let invocation = Invocation {
                owner: &repo.owner,
                principal: &route.principal,
                scratch: &scratch,
                deadline,
                current: &*route.current,
            };
            if self.scope.git.tag(&invocation, &journal.tag())?.is_some() {
                return Err(Error::new("probe tag cleanup is not currently confirmed"));
            }
        }
        self.scope.require(&journal.subject, deadline)?;
        Ok(
            if full
                && (0..journal.repositories.len())
                    .any(|index| !self.fresh_uploads.borrow().contains(&index))
            {
                Finality::Historical
            } else {
                Finality::Fresh
            },
        )
    }

    fn now_ms(&self) -> u64 {
        aterm_time::unix_millis().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests;
