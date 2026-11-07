// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native retained resource custody. Transport authorization belongs to the
//! frontend; actual allocation, birth, execution and recovery belong here.
//! Production serving must run in an independent custodian process so GUI
//! handoff cannot discard original unreaped-child control.

#[cfg(unix)]
mod actor;
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod enrollment;
#[cfg(unix)]
mod forward;
#[cfg(unix)]
mod inspection;
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
#[cfg(target_os = "macos")]
mod subgroup_delegation;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "subgroup_delegation_unsupported.rs"]
mod subgroup_delegation;
#[cfg(unix)]
pub use client::{authorize, capabilities_at, inspect_at, submit};
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
