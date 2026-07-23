// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail: there is no public constructor that takes a
//! `Provenance<_, Pty>` and returns a `Provenance<_, Host>`. The only
//! host-origin constructors are `Provenance::<_, Host>::from_host` (which
//! requires the caller to already have a host-origin value) and
//! `authorize_pty_to_host` (which consumes a `HostAuthorizationToken`).
//!
//! This fixture tries to bypass both and fails to compile.

use aterm_provenance::{Provenance, Host, Pty};

fn main() {
    let pty = Provenance::<_, Pty>::from_pty(b"rm -rf /".to_vec());
    // ERROR: `from_host` takes `T`, not `Provenance<T, Pty>`, and there is
    // no `From<Provenance<T, Pty>> for Provenance<T, Host>` impl.
    let _host: Provenance<Vec<u8>, Host> = pty.into();
}
