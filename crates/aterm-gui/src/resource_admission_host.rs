// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GUI-facing adapter; resource ownership is implemented in the native host.
pub(crate) use aterm_resource_host::{
    authorize, capabilities_at, inspect_at, matches_original_store_at,
};

pub(crate) fn submit(
    submission: aterm_ctl::resource_admission_transport::AuthorizedSubmission,
) -> std::io::Result<()> {
    let _sampling = crate::resource_jobs::native_submission();
    aterm_resource_host::submit(submission)
}

pub(crate) fn submit_accounted(
    submission: aterm_ctl::resource_admission_transport::AuthorizedSubmission,
) -> std::io::Result<()> {
    let _sampling = crate::resource_jobs::native_submission();
    aterm_resource_host::submit_accounted(submission)
}

pub(crate) fn submit_accounted_expected(
    submission: aterm_ctl::resource_admission_transport::AuthorizedSubmission,
    expected: std::os::fd::OwnedFd,
) -> std::io::Result<()> {
    let _sampling = crate::resource_jobs::native_submission();
    aterm_resource_host::submit_accounted_expected(submission, expected)
}
