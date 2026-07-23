// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail: same as `forge_host_from_pty` but for `User`. There is no
//! constructor or conversion that lets `Provenance<_, Pty>` become
//! `Provenance<_, User>`. User-origin data only enters the system through
//! `Provenance::<_, User>::from_user` at input-controller boundaries.

use aterm_provenance::{Provenance, User, Pty};

fn main() {
    let pty = Provenance::<_, Pty>::from_pty(String::from("sudo"));
    // ERROR: there is no `From<Provenance<_, Pty>> for Provenance<_, User>`.
    let _user: Provenance<String, User> = pty.into();
}
