// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Owner-authenticated resource ingress. Policy and launch belong to the host.
use aterm_ctl::resource_admission_transport::{self as transport, AuthorizedSubmission, Peer};
use aterm_types::resource_admission::Submission;
use aterm_uds::CtlStream;
use std::{io, time::Instant};

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
    let authorized = transport::receive(stream, owner_authenticated, deadline, authorize)?;
    submit(authorized)?;
    transport::acknowledge(stream, deadline)
}
