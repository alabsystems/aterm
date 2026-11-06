// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! This platform has no qualified controlled member launch. Never substitute
//! local spawning or unverified descriptor adoption for the native protocol.
use aterm_capacity::build_cpu::{ExternalDebt, Pool};
use aterm_ctl::resource_subgroup_client::Delegations;
use aterm_types::resource_subgroup::Request;
use std::io;
pub(crate) struct Prepared;
fn unavailable() -> io::Error {
    io::ErrorKind::Unsupported.into()
}
impl Prepared {
    pub(crate) fn try_new(
        _: &Pool,
        _: &ExternalDebt,
        _: u32,
        _: &Request,
        _: &Delegations,
    ) -> io::Result<Option<Self>> {
        Err(unavailable())
    }
    pub(crate) fn configure(&mut self, _: &mut aterm_uds::spawnfd::Command) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn bind_child(&mut self, _: u32) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn cpu_slot(&self) -> Option<u32> {
        None
    }
}
