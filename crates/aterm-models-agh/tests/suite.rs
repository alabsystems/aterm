// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The Tier-0 suite of agh's models: one test binary. `autotests = false`: a `tests/*.rs`
//! that is not a `mod` line here never compiles. Run one file's tests with
//! `targo --unverified test -p aterm-models-agh --test suite -- route_wall::`.

mod ensure_once;
mod git_grant;
mod route_wall;
