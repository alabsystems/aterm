// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Owner-authenticated resource ingress. Policy and launch belong to the host.
use aterm_ctl::resource_admission_transport::{self as transport, AuthorizedSubmission, Peer};
use aterm_types::resource_admission::Submission;
use aterm_uds::CtlStream;
use std::{io, time::Instant};

pub(crate) fn attach_accounted_expected(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&Submission, Peer) -> bool,
    matches_backend: impl FnOnce(&std::os::fd::OwnedFd) -> io::Result<bool>,
    submit: impl FnOnce(AuthorizedSubmission, std::os::fd::OwnedFd) -> io::Result<()>,
) -> io::Result<()> {
    let (authorized, expected) = transport::store_expectation::receive_submission(
        stream,
        owner_authenticated,
        deadline,
        authorize,
        matches_backend,
    )?;
    submit(authorized, expected)?;
    transport::store_expectation::acknowledge(stream, deadline)
}

/// Dispatch after consuming exactly `resource-admission-v3 attach`. The
/// dispatcher supplies the exact Instance-principal verdict, never a remote or
/// delegated principal. Session ownership and duplicate-attempt handling remain
/// the host's authorization gate. No allocation or child launch occurs here.
///
/// The callback owns all four descriptors on entry. An attachment ACK means
/// only the callback accepted ownership. Actual launch is a Started event on
/// the private socket, carrying the actual timestamps recorded by the launcher.
pub(crate) fn attach(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&Submission, Peer) -> bool,
    submit: impl FnOnce(AuthorizedSubmission) -> io::Result<()>,
) -> io::Result<()> {
    attach_mode(
        stream,
        owner_authenticated,
        deadline,
        authorize,
        submit,
        false,
    )
}
pub(crate) fn attach_accounted(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&Submission, Peer) -> bool,
    submit: impl FnOnce(AuthorizedSubmission) -> io::Result<()>,
) -> io::Result<()> {
    attach_mode(
        stream,
        owner_authenticated,
        deadline,
        authorize,
        submit,
        true,
    )
}
fn attach_mode(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&Submission, Peer) -> bool,
    submit: impl FnOnce(AuthorizedSubmission) -> io::Result<()>,
    accounted: bool,
) -> io::Result<()> {
    let authorized = if accounted {
        transport::receive_accounted(stream, owner_authenticated, deadline, authorize)
    } else {
        transport::receive(stream, owner_authenticated, deadline, authorize)
    }?;
    submit(authorized)?;
    if accounted {
        transport::acknowledge_accounted(stream, deadline)
    } else {
        transport::acknowledge(stream, deadline)
    }
}
