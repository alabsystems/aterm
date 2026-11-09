// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Actual private host/helper integration; these modules are the production source.
// Off Unix every module and re-export below is compiled out, so nothing is left unused.
#![cfg_attr(
    unix,
    expect(
        unused_imports,
        dead_code,
        reason = "The standalone native entry reuses production modules and their libtest fixture code; libtest-only functions are not registered by this harness=false target"
    )
)]
#[cfg(unix)]
#[path = "../src/actor.rs"]
mod actor;
#[cfg(unix)]
#[path = "../src/bootstrap_graph_transport.rs"]
mod bootstrap_graph_transport;
#[cfg(unix)]
#[path = "../src/fixture_wait.rs"]
mod fixture_wait;
#[cfg(unix)]
use fixture_wait::{HANG as FIXTURE_HANG, deadline as fixture_deadline};
#[cfg(unix)]
#[path = "../src/client.rs"]
mod client;
#[cfg(unix)]
#[path = "../src/enrollment.rs"]
mod enrollment;
#[cfg(unix)]
#[path = "../src/forward.rs"]
mod forward;
#[cfg(unix)]
#[path = "../src/inspection.rs"]
mod inspection;
#[cfg(unix)]
#[path = "../src/linker_transport.rs"]
mod linker_transport;
#[cfg(unix)]
#[path = "../src/metadata_prelude.rs"]
mod metadata_prelude;
#[cfg(unix)]
#[path = "../src/metadata_transport.rs"]
mod metadata_transport;
#[cfg(unix)]
#[path = "../src/resolver_transport.rs"]
mod resolver_transport;
#[cfg(unix)]
#[path = "../src/resource_accounting.rs"]
mod resource_accounting;
#[cfg(unix)]
#[path = "../src/resource_context.rs"]
mod resource_context;
#[cfg(unix)]
#[path = "../src/resource_machine.rs"]
mod resource_machine;
#[cfg(unix)]
#[path = "../src/resource_memory.rs"]
mod resource_memory;
#[cfg(unix)]
#[path = "../src/resource_producer.rs"]
mod resource_producer;
#[cfg(unix)]
#[path = "../src/resource_proxy.rs"]
mod resource_proxy;
#[cfg(unix)]
#[path = "../src/resource_proxy_backend.rs"]
mod resource_proxy_backend;
#[cfg(unix)]
#[path = "../src/resource_proxy_intake.rs"]
mod resource_proxy_intake;
#[cfg(unix)]
#[path = "../src/resource_telemetry.rs"]
mod resource_telemetry;
#[cfg(unix)]
#[path = "../src/runtime.rs"]
mod runtime;
#[cfg(unix)]
#[path = "../src/runtime_input_transport.rs"]
mod runtime_input_transport;
#[cfg(target_os = "macos")]
#[path = "../src/subgroup_delegation.rs"]
mod subgroup_delegation;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "../src/subgroup_delegation_unsupported.rs"]
mod subgroup_delegation;
#[cfg(unix)]
#[path = "../src/workload_transport.rs"]
mod workload_transport;
#[cfg(unix)]
pub use client::{authorize, capabilities_at, inspect_at, submit};
#[cfg(unix)]
pub use enrollment::answer as answer_enrollment;
#[cfg(unix)]
pub use inspection::lookup as inspect_native;

#[cfg(target_os = "macos")]
#[path = "../src/resource_native_fixture.rs"]
mod native_fixture;
#[cfg(target_os = "macos")]
pub use native_fixture::entry as native_member_fixture;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    native_member_fixture(&std::env::args_os().skip(1).collect::<Vec<_>>())
}
// Member descriptor custody is currently implemented on macOS; other hosts
// compile the shared production modules but do not claim this native control.
#[cfg(not(target_os = "macos"))]
fn main() {}
