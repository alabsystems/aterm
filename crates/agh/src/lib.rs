// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]
#![forbid(unsafe_code)]
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![warn(missing_docs)]

//! `agh`: `aterm`'s GitHub client with routed identities, typed outcomes, and checked downloads.

pub mod classify;
pub mod cli;
pub mod client;
pub mod config;
pub mod download;
pub mod error;
pub mod fx;
pub mod git;
pub mod github;
pub mod identity;
pub mod journal;
pub mod model;
pub mod needs;
pub mod notify;
pub mod page;
pub mod path;
pub mod policy;
pub mod rate;
pub mod redact;
pub mod retry;
pub mod slug;
#[cfg(test)]
mod test_fixtures;
pub mod wait;
pub mod wire;
pub mod write;

pub use client::{Accept, Answered, Call, Github, GithubBuilder, Meter, Streamed};
pub use error::{Error, ErrorData, Kind, Replay, Result, SideEffect, Sub};
pub use fx::Fx;
pub use github::How;
pub use journal::{IntentId, IntentStore, Journal};
pub use policy::{Permit, PermitTarget, Provenance};
pub use slug::{ContentRef, RepoArg, Slug};
pub use write::{Deleted, Effect, Ensured, Outcome};

/// agh's version: the workspace version, printed by `agh version` and sent in `User-Agent`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The REST API version agh pins (`X-GitHub-Api-Version`), supported until at least 2028-03-10.
pub const API_VERSION: &str = "2022-11-28";
