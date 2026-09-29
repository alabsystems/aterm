// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release` — the release cutter behind the `targo --unverified ship` alias
//! (release spec "aterm release+update v2").
//!
//! One binary owns the whole cut: pre-claim gates → build-number ledger claim
//! (fetch/push compare-and-swap on `RELEASES.ledger`) → universal build with
//! `SOURCE_DATE_EPOCH=n` → .app bundle → sign → DMG → manifest → tag → ONE
//! publication onto the release channel, made the head last. It is run
//! via the `.cargo/config.toml` alias (`ship = "run -q --release -p
//! aterm-release --"`), never `cargo install` — a stale installed binary must
//! not be able to cut a release (spec decision 13).
//!
//! Binary-only on purpose — there is no `lib.rs`. Nothing outside the cut tool
//! may link this code (the release spec's §9 file plan), and because every
//! module below is a private `mod` of this binary, rustc's `dead_code` judges
//! every item against what `main()` can reach: code that only a test calls
//! fails `targo-tippy --all-targets -- -D warnings`. The integration tests
//! reach the modules by `#[path]`-mounting them into one test binary,
//! `tests/it/main.rs`; Cargo.toml says why that binary, not this one, runs the
//! inline unit tests.
//!
//! Module map (one module per pipeline stage; each doc comment cites its spec
//! section):

// This machine's OWN Developer ID identity: keypair and CSR born here, certificate
// imported here, no private key ever crossing a machine boundary. Apple permits no
// automated path to a Developer ID certificate, so this is the shortest one it allows.
mod apple;
mod buildplan;
mod bundle;
mod changelog;
// The release channel: the one release object a cut publishes onto, its exact asset
// set, and the order and PATCH that make it the head.
mod channel;
mod cli;
mod dmg;
mod gates;
mod ledger;
// The producer half of the machine-roster tier: this machine's public identity, the
// cut-time authorization gate, and the attribution stamp. Deliberately NOT in sign.rs or
// publish.rs — it touches no secret and no upload.
mod machines;
mod manifest_out;
// One command from fresh checkout to publishing machine: seed the roster pair from the
// channel release, drive the atpkg-keys join in-process, audit the Apple/token stack.
mod provision;
mod publish;
mod sign;
mod verify;

fn main() {
    // cli::run() owns arg parsing (hand-rolled std::env::args, spec §5),
    // subcommand dispatch and the exit code; main() stays this thin forever
    // so the whole surface is testable through cli.
    std::process::exit(cli::run());
}
