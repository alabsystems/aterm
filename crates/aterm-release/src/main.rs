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
// The release's bundle, built ONCE and kept in the release cache by what it was built
// as: what MEASURE judges and what the cut signs a byte copy of.
mod artifact;
mod buildplan;
mod bundle;
mod changelog;
// The release channel: the one release object a cut publishes onto, its exact asset
// set, and the order and PATCH that make it the head.
mod channel;
mod cli;
mod dmg;
// The cutter's durability flushes, in one place (and a plain fsync under its tests).
mod durable;
// The release's own handoff fixtures: written before its claim, landed on main after
// its cut, so the next cut finds them.
mod fixtures;
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
// `ship release`: the ONE release command — a journaled, resumable step machine over
// the cutter's own judges, the publication engine and the cut.
mod release;
mod release_steps;
mod sign;
// The source a release builds, made from git objects into a per-clone cache, its
// dependencies vendored and checked against Cargo.lock, digested and re-verified.
mod snapshot;
// The whole-tree Trust lane's facts and their ONE judge: the lane's end, the
// cutter's and `ship check`'s TRUST gate, and `ship trust verdict` for the
// release scripts (FACTS, NOT VERDICTS).
mod trust_facts;
// Per gated library, a key over what its check read: recorded in the facts
// for measuring, never judged (W4, docs/measured/RETRO-release-0.96-0.100-2026-09-30.md).
mod trust_closure;
// The candidate gate (D27): a candidate toolchain's whole-tree run against the
// published seal's, library by library, from what each measured.
mod trust_compare;
// Its per-obligation records (the lane reduces each check's report.ndjson), keys
// and retraction ledger: every obligation the seal proved, not a count.
mod trust_oblig;
mod verify;

fn main() {
    // cli::run() owns arg parsing (hand-rolled std::env::args, spec §5),
    // subcommand dispatch and the exit code; main() stays this thin forever
    // so the whole surface is testable through cli.
    std::process::exit(cli::run());
}
