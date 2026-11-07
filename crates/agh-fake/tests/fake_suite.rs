// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh-fake's own tests: one binary (autotests = false); a tests/*.rs that is not a mod line
//! here never compiles. Each test starts its own `FakeGithub`.

mod faults;
#[cfg(unix)]
mod git_backend;
mod git_http;
mod raw;
mod routes;
mod writes;
