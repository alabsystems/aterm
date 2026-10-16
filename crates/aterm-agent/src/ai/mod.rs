// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AI CONNECTIONS: one working login per AI CLI aterm manages, on every machine, a doctor
//! that says which one needs a person, a preferred CLI, and reviews one AI asks of another
//! (docs/DESIGN-ai-connections-2026-10-16.md).
//!
//! The stance is Claude accounts v2's, for every vendor: one login per vendor per machine,
//! made through the vendor's own flow; aterm never reads, stores, copies or relays a
//! credential and reads no keychain (it may learn that a login file exists and when the
//! vendor last wrote it); nothing runs under an idle login to check it; no synthetic
//! traffic, since real use is the check. Accounts v2 keeps owning Claude's login: this layer
//! reads it, signs Claude in only through its renewal, and raises nothing it raises. Code
//! goes to another vendor only to the CLIs the owner listed in `[ai] reviewers`.
//!
//! * [`connector`] — the one row per AI CLI, keyed by atpkg's `VENDORS` word.
//! * [`config`] — the `[ai]` table and the fleet document.
//! * [`facts`] — what the doctor reads of the machine, and the child-process seam.
//! * [`state`] — one word per CLI, and the health: pure.
//! * [`store`] — what reviews and sign-ins have shown of each CLI's login.
//! * [`login`] — sign-in through each vendor's own flow (Claude's through accounts v2).
//! * [`change`] and [`review`] — reviews across AIs.
//! * [`run`] — the private run directory a sign-in or a reviewer starts in.
//! * [`cli`] — `aterm ai`.

pub mod change;
pub mod cli;
pub mod config;
pub mod connector;
pub mod facts;
pub mod login;
pub mod review;
pub mod run;
pub mod state;
pub mod store;

#[cfg(test)]
pub(crate) mod testing;
