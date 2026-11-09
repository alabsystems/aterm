// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The distinct Bootstrap SDK query owner. Request/Prepared/Terminal are DATA;
//! only the original Job and its actual supplementary GNU borrower may launch
//! this native target. No root-implicit-unit transfer or pause is inferred.

use super::{journal, sdk, spawnfd};
use std::{io, time::Instant};
use trust_resource_controller::bootstrap_graph::query as wire;

#[path = "resource_admission_bootstrap_query/capture.rs"]
mod capture;
#[path = "resource_admission_bootstrap_query/delivery.rs"]
mod delivery;
#[path = "resource_admission_bootstrap_query/execution.rs"]
mod execution;
#[path = "resource_admission_bootstrap_query/preparation.rs"]
mod preparation;
pub(super) use execution::{ClosedFailed, Completed, NativeClosed, Query, Retained};

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

#[cfg(test)]
#[path = "resource_admission_bootstrap_query/tests.rs"]
mod tests;
