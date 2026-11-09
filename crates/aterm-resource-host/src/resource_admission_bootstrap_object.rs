// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Distinct object-producing Bootstrap operation, paid only by a real original
//! Epoch loan. Decoded source/output observations never construct their owners.
use super::{journal, sdk, spawnfd};
use std::{io, time::Instant};
use trust_resource_controller::bootstrap_graph::object as wire;
#[path = "resource_admission_bootstrap_object/capture.rs"]
mod capture;
#[path = "resource_admission_bootstrap_object/delivery.rs"]
mod delivery;
#[path = "resource_admission_bootstrap_object/execution.rs"]
mod execution;
#[path = "resource_admission_bootstrap_object/output.rs"]
mod output;
#[path = "resource_admission_bootstrap_object/preparation.rs"]
mod preparation;
pub(crate) use execution::SourceOffer;
pub(super) use execution::{ClosedFailed, Completed, NativeClosed, Object, Retained};
pub(super) use preparation::observed_file as observed_program;
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

#[cfg(all(test, target_os = "macos"))]
#[path = "resource_admission_bootstrap_object/inheritance_tests.rs"]
mod inheritance_tests;
