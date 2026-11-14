// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated native owner routing. No token is exported to the probe journal or argv.

use super::{
    http::{Current, Http},
    model::Journal,
    scope::Scope,
};
use crate::{
    Error, Result,
    client::{self, EntrySnapshot, VerifiedEntry},
};
use std::sync::Arc;
use std::time::Instant;

pub(super) struct Route {
    pub http: Http,
    pub principal: String,
    pub current: Current,
}

fn guarded(
    scope: &Scope,
    journal: &Journal,
    snapshot: &EntrySnapshot,
    deadline: Instant,
) -> Current {
    let scope = scope.clone();
    let subject = journal.subject.clone();
    let snapshot = snapshot.clone();
    Arc::new(move || {
        super::diagnosed(
            "credential-current-admission",
            scope.require(&subject, deadline),
        )?;
        if super::diagnosed(
            "credential-current-material",
            snapshot.read_publication(&scope.paths),
        )?
        .is_none()
        {
            super::diagnostic("credential-current-missing", &Error::new("changed"));
            return Err(Error::new("probe GitHub credential or audience changed"));
        }
        Ok(())
    })
}

fn credential(
    scope: &Scope,
    journal: &Journal,
    name: &str,
    deadline: Instant,
) -> Result<(VerifiedEntry, Http, Current)> {
    let config = super::diagnosed(
        "credential-admission",
        scope.require(&journal.subject, deadline),
    )?;
    let entry = super::diagnosed(
        "credential-material",
        client::entry::read_publication_material(&scope.paths, &config, name),
    )?
    .ok_or_else(|| Error::new("probe awaits an authenticated materialized GitHub credential"))?;
    let current = guarded(scope, journal, entry.snapshot(), deadline);
    let http = super::diagnosed(
        "credential-client",
        Http::new(entry.value(), deadline, current.clone()),
    )?;
    Ok((entry, http, current))
}

pub(super) fn route(
    scope: &Scope,
    journal: &Journal,
    index: usize,
    deadline: Instant,
) -> Result<Route> {
    let owner = &journal.repositories[index].owner;
    let (dev_entry, dev, dev_current) = super::diagnosed(
        "development-credential",
        credential(scope, journal, crate::entry_class::DEV_GH_TOKEN, deadline),
    )?;
    let dev_login = super::diagnosed("development-identity", dev.identity())?;
    let selected = if owner.eq_ignore_ascii_case(aterm_types::channel::PUBLIC_OWNER) {
        let (release_entry, release, current) = super::diagnosed(
            "release-credential",
            credential(
                scope,
                journal,
                crate::entry_class::RELEASE_GH_TOKEN,
                deadline,
            ),
        )?;
        let release_login = super::diagnosed("release-identity", release.identity())?;
        if dev_entry.value().as_slice() == release_entry.value().as_slice()
            || release_login.eq_ignore_ascii_case(&dev_login)
        {
            return Err(Error::new(
                "probe requires distinct native development and release credentials",
            ));
        }
        Route {
            http: release,
            principal: release_login,
            current,
        }
    } else if owner.eq_ignore_ascii_case(&dev_login) {
        Route {
            http: dev,
            principal: dev_login,
            current: dev_current,
        }
    } else {
        return Err(Error::new(
            "probe repository is outside the native owner routes",
        ));
    };
    if journal.repositories[index]
        .principal
        .as_ref()
        .is_some_and(|old| old != &selected.principal)
    {
        return Err(Error::new(
            "probe GitHub writer changed since its recorded receipt",
        ));
    }
    super::diagnosed("repository-permission", selected.http.admits(owner))?;
    super::diagnosed("repository-current", (selected.current)())?;
    Ok(selected)
}

pub(super) fn notary_profile(
    scope: &Scope,
    journal: &Journal,
    deadline: Instant,
) -> Result<String> {
    scope.require(&journal.subject, deadline)?;
    let modern = scope.paths.home.join(super::super::RELEASE_PROFILE);
    let bytes = crate::fsx::read_private(&modern, super::super::MAX_READ)?
        .ok_or_else(|| Error::new("probe has no authorized release profile"))?;
    // Parse only the public profile-name assignment. Do not copy the profile's private
    // signing key into an extra TOML value merely to read one nonsecret field.
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new("probe release profile is not UTF-8"))?;
    parse_notary_profile_text(text)
}

fn parse_notary_profile_text(text: &str) -> Result<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        let Some((name, rhs)) = line.split_once('=') else {
            return Err(Error::new("probe release profile is malformed"));
        };
        if rhs.contains("\"\"\"") || rhs.contains("'''") {
            return Err(Error::new("probe release profile is malformed"));
        }
        if name.trim().trim_matches(['"', '\'']) != "notary_profile" {
            continue;
        }
        if found.is_some() {
            return Err(Error::new("probe notary profile name is duplicated"));
        }
        let value: aterm_toml::Value = aterm_toml::from_str(line)
            .map_err(|_| Error::new("probe notary profile name is malformed"))?;
        let profile = value
            .get("notary_profile")
            .and_then(aterm_toml::Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 128
                    && !s.starts_with('-')
                    && s.bytes().all(|b| b >= 0x20 && b != 0x7f)
            })
            .ok_or_else(|| Error::new("probe notary profile name is invalid"))?;
        found = Some(profile.to_owned());
    }
    found.ok_or_else(|| Error::new("probe release profile has no notary profile"))
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
