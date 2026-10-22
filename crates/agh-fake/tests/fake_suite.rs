// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh-fake's own tests: one binary (autotests = false); a tests/*.rs that is not a mod line
//! here never compiles. Each test starts its own `FakeGithub`.

mod faults;
mod raw;
mod routes;
