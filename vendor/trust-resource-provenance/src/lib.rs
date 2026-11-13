// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Native provenance for the canonical retained custodian.
//! Discovery never creates account state or grants resources. A selected live
//! process, immutable image, original socket and actual native ancestry remain
//! checked through the caller's bounded exchange; JSON and paths alone confer
//! no authority.
//! Explicit produced-output owners can additionally create bounded new failure
//! evidence beneath their original output directory. Discovery and SDK/source
//! inputs remain read-only; evidence bytes establish no execution or proof.

#[cfg(unix)]
mod connection;
#[cfg(unix)]
mod private;
#[cfg(unix)]
pub use private::{require_no_acl_entries, require_no_acl_grants};
#[cfg(unix)]
pub mod process;
#[cfg(unix)]
pub mod program;
#[cfg(unix)]
mod reader;
#[cfg(unix)]
pub mod schema;
#[cfg(unix)]
pub use reader::{
    Host, HostControlTransfer, Instance, ORIGINAL_CONTROL_DESCRIPTORS, PinnedHelper, PinnedProgram,
    Store,
};
