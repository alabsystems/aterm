// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actor-flag eligibility for numeric-group memory attribution. This decision
//! supplies neither native identity custody nor scope-closure authority.

pub const fn may_attribute_group(reaped: bool, retired: bool) -> bool {
    !reaped && !retired
}
