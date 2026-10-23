// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! tools/install.sh's Linux half over the static bootstrap verifier: the corpus and the
//! fixtures the embedded Python verifier is held to (`chain`, shared with
//! crates/aterm-update/tests/install_sh by path), run through install.sh's own glue under
//! its test seam (`verifier`); the pin, the seam and the dispatch (`seam`); and the
//! shipped static build, which the Linux guest lane hands in (`shipped`).
//!
//! `targo --unverified test -p aterm-bootstrap-verify --test install_sh`
//!
//! Unix only: the script is bash, and the tests lay unix stand-ins and modes.
#![cfg(unix)]

#[path = "../../../aterm-update/tests/install_sh/chain.rs"]
#[allow(
    dead_code,
    reason = "shared with the embedded verifier's suite, which uses all of it"
)]
mod chain;
mod seam;
mod shipped;
#[path = "../../../aterm-update/tests/install_sh/support.rs"]
#[allow(
    dead_code,
    reason = "the installer suite's support, of which this suite uses a part"
)]
mod support;
mod verifier;
