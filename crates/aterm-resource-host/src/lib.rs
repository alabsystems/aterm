// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native retained resource custody. Transport authorization belongs to the
//! frontend; actual allocation, birth, execution and recovery belong here.
//! Production serving must run in an independent custodian process so GUI
//! handoff cannot discard original unreaped-child control.

// The lifecycle fixtures judge custody and journal ordering. The durable crate's own
// tests judge the full hardware flush; these private worlds keep every flush call as fsync.
#[cfg(all(test, unix))]
aterm_durable::plain_durable_flushes_in_tests!();

#[cfg(all(test, unix))]
mod fixture_wait;
#[cfg(all(test, unix))]
use fixture_wait::{HANG as FIXTURE_HANG, deadline as fixture_deadline};

#[cfg(unix)]
mod actor;
#[cfg(unix)]
mod bootstrap_graph_transport;
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod enrollment;
#[cfg(unix)]
mod forward;
#[cfg(unix)]
mod inspection;
#[cfg(unix)]
mod linker_transport;
#[cfg(unix)]
mod metadata_prelude;
#[cfg(unix)]
mod metadata_transport;
#[cfg(unix)]
mod resolver_transport;
#[cfg(unix)]
mod resource_accounting;
#[cfg(unix)]
mod resource_context;
#[cfg(unix)]
mod resource_machine;
#[cfg(unix)]
mod resource_memory;
#[cfg(unix)]
mod resource_producer;
#[cfg(unix)]
mod resource_proxy;
#[cfg(unix)]
mod resource_proxy_backend;
#[cfg(unix)]
mod resource_proxy_intake;
#[cfg(unix)]
mod resource_telemetry;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
mod runtime_input_transport;
#[cfg(target_os = "macos")]
mod subgroup_delegation;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "subgroup_delegation_unsupported.rs"]
mod subgroup_delegation;
#[cfg(unix)]
mod workload_transport;
#[cfg(unix)]
pub use client::{
    authorize, capabilities_at, inspect_at, matches_original_store_at, submit, submit_accounted,
    submit_accounted_expected,
};
#[cfg(unix)]
pub use enrollment::answer as answer_enrollment;
#[cfg(unix)]
pub use inspection::lookup as inspect_native;
/// Dispatch private native roles before initializing the GUI or ordinary CLI.
#[cfg(unix)]
pub fn entry(args: &[std::ffi::OsString]) -> Option<std::process::ExitCode> {
    if let Some(code) =
        resource_proxy::entry(args, &mut resource_proxy_backend::NativeBackend::account())
    {
        return Some(code);
    }
    runtime::entry(args)
}

#[cfg(all(test, target_os = "macos"))]
#[path = "resource_native_fixture.rs"]
mod native_fixture;
#[cfg(all(test, target_os = "macos"))]
pub use native_fixture::entry as native_member_fixture;

#[cfg(all(test, target_os = "macos"))]
mod metadata_native_test_entry;

// The actual private protocol is exercised before any Solver recipe can select it.
#[cfg(all(test, target_os = "macos"))]
mod solver_bootstrap;
