// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release` — the release cutter behind the `cargo ship` alias
//! (release spec "aterm release+update v2").
//!
//! One binary owns the whole cut: pre-claim gates → build-number ledger claim
//! (fetch/push compare-and-swap on `RELEASES.ledger`) → universal build with
//! `SOURCE_DATE_EPOCH=n` → .app bundle → sign → DMG → manifest → draft-first
//! GitHub publish with a late tag → cask pin → post-publish verify. It is run
//! via the `.cargo/config.toml` alias (`ship = "run -q --release -p
//! aterm-release --"`), never `cargo install` — a stale installed binary must
//! not be able to cut a release (spec decision 13).
//!
//! Module map (one module per pipeline stage; each doc comment cites its spec
//! section):

mod buildplan;
mod bundle;
mod changelog;
mod cli;
mod dmg;
mod gates;
mod ledger;
mod manifest_out;
mod mirror;
mod publish;
mod seedpack;
mod sign;
mod verify;

fn main() {
    // cli::run() owns arg parsing (hand-rolled std::env::args, spec §5),
    // subcommand dispatch and the exit code; main() stays this thin forever
    // so the whole surface is testable through cli.
    std::process::exit(cli::run());
}
