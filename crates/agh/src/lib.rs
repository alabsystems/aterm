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

/// Names each frozen crate-private item whose first caller a later package writes, so the
/// plain library's `dead_code` lint passes at every commit until then (plan §1, rule 14).
/// It runs nothing and nothing calls it. No package edits it after WP0b; WP14 deletes it.
#[doc(hidden)]
pub fn frozen_roots() {
    let _ = policy::Router::permit_decided;
    let _ = policy::Permit::named;
    let _ = config::edit_writers;
    let _ = identity::Profile::set_legacy_loose;
    let _ = identity::provision::add_with_metered;
    let _ = identity::import::import_from_gh_metered;
    let _ = Call::write
        as fn(
            wire::Method,
            String,
            policy::RouteRequest,
            retry::Idempotency,
            client::CallBody,
        ) -> Call;
    let _ = |call: &Call| call.body.clone();
    let _ = Github::call_write;
    let _ = Github::call_write_guarded;
    let _ = Github::intent_draft;
    let _ = Github::one_shot;
    let _ = Github::note_notify;
    let _ = write::settle_table;
    let _ = journal::Journal::taped;
    let _ = github::Releases::create_step;
    let _ = github::Releases::send_create;
    let _ = github::Releases::settle_create;
    let _ = github::Releases::discover;
    let _ = git::credential::answer;
    let _ = |asked: &git::credential::Asked<'_>| {
        asked.stdin_is_terminal
            || asked.nonce.is_some()
            || asked.loopback.is_some()
            || asked.now > 0
    };
    let _ = git::grant::write;
    let _ = git::grant::remove;
    let _ = git::grant::sweep;
    let _ = notify::settled;
}
