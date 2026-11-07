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
    pub(crate) fn child_targets(&self) -> io::Result<Vec<u32>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn from_workload_original(
        _: &ExternalDebt,
        _: &Request,
        _: &Delegations,
        _: Option<[u32; 2]>,
        _: aterm_capacity::build_cpu::Lease,
    ) -> io::Result<Self> {
        Err(unavailable())
    }
    pub(crate) fn try_new_mapped(
        _: &Pool,
        _: &ExternalDebt,
        _: u32,
        _: &Request,
        _: &Delegations,
        _: Option<[u32; 2]>,
    ) -> io::Result<Option<Self>> {
        Err(unavailable())
    }
    pub(crate) fn retain_guard(&mut self, _: std::os::fd::OwnedFd) -> io::Result<u32> {
        Err(unavailable())
    }
    pub(crate) fn configure(&mut self, _: &mut aterm_uds::spawnfd::Command) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn configure_with_extra(
        &mut self,
        _: &mut aterm_uds::spawnfd::Command,
        _: Vec<(std::os::fd::OwnedFd, i32)>,
    ) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn bind_child(&mut self, _: u32) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn configure_linker(
        &mut self,
        _: &mut aterm_uds::spawnfd::Command,
        _: Vec<(std::os::fd::OwnedFd, i32)>,
    ) -> io::Result<()> {
        Err(unavailable())
    }
    pub(crate) fn cpu_slot(&self) -> Option<u32> {
        None
    }
}
