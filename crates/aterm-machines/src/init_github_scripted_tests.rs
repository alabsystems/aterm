// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

pub(crate) use super::native::tests::{metadata, owned_source, repository};
/// `user`'s account on `sys` with one repository of hers, `name`, already a worktree in
/// `~`: the stage init runs with no init.toml, converged.
pub(crate) fn converged(sys: &crate::port::Fake, user: &crate::model::User, name: &str) {
    super::native::tests::converged(sys, user, name);
}
