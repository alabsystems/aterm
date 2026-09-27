// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail: same as `forge_host_from_pty` but for `User`. There is no
//! constructor or conversion that lets `Provenance<_, Pty>` become
//! `Provenance<_, User>` (and, since 2026-09-25, no `User` constructor at all —
//! only `from_host` and `from_pty` ship).

use aterm_provenance::{Provenance, Pty, User};

fn main() {
    let pty = Provenance::<_, Pty>::from_pty(String::from("sudo"));
    // ERROR: there is no `From<Provenance<_, Pty>> for Provenance<_, User>`.
    let _user: Provenance<String, User> = pty.into();
}
