// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Finite `int32-exact/v1` compute jobs on Metal shared buffers, each result a steer compared bit for bit with aterm-effects' CPU reference, never evidence (design §12).

mod device;
mod job;
mod pipeline;
#[cfg(test)]
mod tests;

#[cfg(debug_assertions)]
pub use device::Fault;
pub use device::{
    ComputeDevice, Leaks, Limits, OpenError, TooLarge, What, current_allocated_size_fresh,
};
pub use job::{
    Completed, HANG_DETECTOR, InFlight, Job, JobFailure, JobState, JobTimes, LoadRefusal, Params,
    ParamsRefusal, Prepared, SubmitRefusal, Timebase, ticks,
};
pub use pipeline::{
    KERNEL, Library, Mismatch, PIPELINE_DOMAIN, Pipeline, PipelineError, RUN_SALT_MIN, Shape,
    identity, prelude, source,
};
