// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the supervise conformance binds share (`agent_suite`'s modules): the
//! walks stay per model.

use aterm_spec::derive::Model;

/// The model's constant `name`; a name it does not declare panics.
pub fn cnst(m: &Model, name: &str) -> i64 {
    m.consts
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("no const {name}"))
}
