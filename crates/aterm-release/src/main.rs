// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release`: the release cutter behind the `targo --unverified ship` alias.
//!
//! One binary owns the cut: pre-claim gates, build-number ledger claim (fetch/push
//! compare-and-swap on `RELEASES.ledger`), universal build with `SOURCE_DATE_EPOCH=n`,
//! .app bundle, sign, DMG, manifest, tag, and one publication onto the release channel,
//! made the head last. It runs from source through the `.cargo/config.toml` alias, never
//! an installed copy, so a stale binary cannot cut a release.
//!
//! Binary-only on purpose (no `lib.rs`): nothing else may link the cutter, and because
//! every module is a private `mod`, `dead_code` judges each item against what `main()`
//! reaches, so code only a test calls fails tippy. The integration tests `#[path]`-mount
//! the modules into `tests/it/main.rs`; Cargo.toml says why that binary runs the inline
//! unit tests.

// `apple`: this machine's own Developer ID identity. Keypair and CSR are born here and the
// certificate imported here; no private key crosses a machine boundary. Apple permits no
// automated path to a Developer ID certificate, so this is the shortest one it allows.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the release cutter, run from source"
)]
mod apple;
// The release's bundle, built once and kept in the release cache by what it was built as:
// what `verify release` judges and what the cut signs a byte copy of.
mod artifact;
mod buildplan;
mod bundle;
mod changelog;
// The release channel: the one release object a cut publishes onto, its asset set, and
// the order that makes it the head.
mod channel;
// The one clean-tree judge: which paths of a checkout differ from its HEAD.
mod clean;
mod cli;
// A git credential helper that waits for a person, ended: nothing the release starts
// waits on one.
mod credential_wait;
// Which commit this running cutter is the build of, judged as it starts.
mod cutter;
// `ship dev-app`: this checkout built into `aterm (dev).app`, signed locally, installed
// beside the release.
mod dev_app;
// The self-signed "aterm dev" identity a dev build signs with, so its macOS grants
// survive a rebuild.
mod dev_identity;
mod dmg;
// The cutter's durability flushes.
mod durable;
// The release's own handoff fixtures, landed on main after its cut for the next cut.
mod fixtures;
mod gates;
// `ship hook`: the publication engine's pre-promote, post-promote and transforms hooks.
mod hook;
// The release's interrupt: SIGINT, SIGTERM and SIGHUP caught, handed to a running
// step's group member by member, never to a trustd.
mod interrupt;
mod ledger;
// The producer half of the machine-roster tier: this machine's public identity, the
// cut-time authorization gate and the attribution stamp. Kept out of sign.rs and
// publish.rs: it touches no secret and no upload.
mod machines;
mod manifest_out;
// `ship provision`: fresh checkout to publishing machine in one command.
mod provision;
mod publish;
// `ship release`: the one release command, a journaled, resumable step machine over the
// cutter's own judges, the publication engine and the cut.
mod release;
mod release_steps;
mod sign;
// `ship site`: alab.systems brought to a release: the downloads elected from the channel,
// the pages and `/releases` rendered, the `/terminal` engine rebuilt, then deployed.
mod site;
// `ship tap deploy`: distribution/homebrew-tap mirrored into a checkout of the public
// tap and committed there, each live cask's version and sha256 kept; the push is the
// owner's.
mod tap;
// The source a release builds, made from git objects into a per-clone cache, its
// dependencies vendored and checked against Cargo.lock, digested and re-verified.
mod snapshot;
// The export's source rewrites the publication engine runs at `pub stage`.
mod transforms;
// The whole-tree Trust lane's facts and shared judge: the lane's end and the
// explicit `ship trust judge` command.
mod trust_facts;
// The lane itself (`ship trust run`): the gate list, the slots, the prover's
// process trees and trustd, then the judge.
mod trust_lane;
// The lane's per-obligation records (`--obligations`: each check's report.ndjson
// reduced beside its log).
mod trust_oblig;
mod verify;

fn main() {
    // Which commit this process is the build of, before any step moves a checkout.
    cutter::judge_at_start();
    // cli::run owns arg parsing, dispatch and the exit code, so the whole surface is
    // testable through cli.
    std::process::exit(cli::run());
}
