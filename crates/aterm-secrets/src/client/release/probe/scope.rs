// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Existing default-ALab membership plus the independent release ring; no enrollment.

use super::{model::Subject, port::Git};
use crate::{
    Error, Result,
    client::{self, Config, Paths},
    commons::bootstrap::Bootstrap,
};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub(super) struct Scope {
    pub paths: Paths,
    pub git: Arc<dyn Git>,
}

impl Scope {
    pub(super) fn current(&self, deadline: Instant) -> Result<(Subject, Config)> {
        if Instant::now() >= deadline {
            return Err(Error::new("release probe admission deadline"));
        }
        if crate::fsx::kind(&crate::service::subscriber_off_path(&self.paths.home))?
            != crate::fsx::Kind::Missing
        {
            return Err(Error::new(
                "release probe requires enabled default-ALab membership",
            ));
        }
        let config = client::config(&self.paths)?
            .filter(|c| client::alab_vault(c) && c.name == crate::ALAB_VAULT)
            .ok_or_else(|| {
                Error::new("release probe requires authenticated default-ALab configuration")
            })?;
        let (bootstrap, journal) =
            Bootstrap::established_with_journal_until(&self.paths, deadline)?;
        bootstrap.revalidate_custody()?;
        let access = bootstrap
            .access_in(&journal)?
            .filter(|access| access.is_current(&journal))
            .ok_or_else(|| Error::new("release probe requires current admitted membership"))?;
        // Publication is authorized by membership and the release ring, independently of
        // whether this member's application-table role is Reader or Writer.
        let binding = access.local().binding();
        let node = self.git.node()?;
        if !super::model::hex(&node.id, 32) {
            return Err(Error::new("release probe has no existing fleet identity"));
        }
        let (log, _) = client::trusted_commons_log(&self.paths, bootstrap.trust())?
            .ok_or_else(|| Error::new("release probe requires authenticated membership history"))?;
        let membership = log.membership();
        if membership.node(&node.key)
            != crate::membership_log::NodeStanding::Member(binding.actor())
            || membership
                .get(&binding.actor())
                .is_none_or(|member| &member.binding != binding)
        {
            return Err(Error::new(
                "release probe fleet key does not match the current member",
            ));
        }
        if !super::super::authorized_ring_readonly(&self.paths) {
            return Err(Error::new(
                "release probe has no current release-ring authority",
            ));
        }
        bootstrap.revalidate_custody()?;
        if !access.is_current(&journal) || self.git.node()? != node {
            return Err(Error::new(
                "release probe membership or fleet identity changed during admission",
            ));
        }
        if client::config(&self.paths)?.as_ref() != Some(&config)
            || crate::fsx::kind(&crate::service::subscriber_off_path(&self.paths.home))?
                != crate::fsx::Kind::Missing
        {
            return Err(Error::new(
                "release probe membership changed during admission",
            ));
        }
        let trust = bootstrap.trust();
        if Instant::now() >= deadline {
            return Err(Error::new("release probe admission deadline"));
        }
        let encode = aterm_codec::hex::encode_lower;
        Ok((
            Subject {
                uid: crate::fsx::euid(),
                node: node.id,
                member: encode(binding.machine()),
                writer: encode(binding.writer()),
                incarnation: encode(binding.incarnation()),
                network: encode(&trust.network),
                authority: encode(&trust.authority),
                genesis: encode(&trust.genesis),
                vault: encode(&client::source_identity(&config)),
            },
            config,
        ))
    }

    pub(super) fn require(&self, expected: &Subject, deadline: Instant) -> Result<Config> {
        let (subject, config) = self.current(deadline)?;
        if &subject != expected {
            return Err(Error::new("release probe member or source changed"));
        }
        Ok(config)
    }
}
