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
mod resource_memory;
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
#[cfg(unix)]
pub use runtime::entry;
