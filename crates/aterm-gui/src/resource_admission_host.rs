// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GUI-facing adapter; resource ownership is implemented in the native host.
pub(crate) use aterm_resource_host::{
    authorize, capabilities_at, inspect_at, matches_original_store_at, submit, submit_accounted,
    submit_accounted_expected,
};
