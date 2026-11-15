// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual original Root archive-copy operation. Reports remain DATA; only the
//! retained native issuer can close its work after real retirement and return.
//! The completed copy grants no Seed, SDK adoption or availability. No-fork
//! validation supplies no general filesystem or macro containment. Host death
//! and reconnect cannot reconstruct this live issuer from journal/status DATA;
//! uncertain custody stays withheld rather than inventing a completion.
use super::{bootstrap_capture::Capture, journal, sdk, spawnfd};
use std::{io, time::Instant};
use trust_resource_controller::bootstrap_graph::archive as wire;
#[path = "resource_admission_bootstrap_archive/execution.rs"]
mod execution;
#[path = "resource_admission_bootstrap_archive/preparation.rs"]
mod preparation;
#[path = "resource_admission_bootstrap_archive/io.rs"]
mod transport;
pub(super) use execution::{Archive, NativeClosed, Retained};
pub(super) use preparation::observed_program;
fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why)
}
fn alive(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
