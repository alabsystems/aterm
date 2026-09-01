<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Contributing

This repository is a public source snapshot of aterm's private development
line, cut at release time. Focused fixes and well-scoped improvements are
welcome. The public tree includes the whole Rust workspace;
[PUBLICATION.md](PUBLICATION.md) lists exactly what the export leaves out —
release credentials, prebuilt binaries, internal operational notes and proof
packets.

## Build and test

The snapshot pins a stock Rust toolchain, so no ALab tooling is needed to build
it: `rust-toolchain.toml` names the pinned version and rustup will fetch it. On
macOS you also need the Xcode Command Line Tools; Linux and Windows build and
test from source too, though only macOS has released binaries, an installer,
and the self-updater. From the workspace root:

```sh
cargo check --locked -p aterm
cargo test --locked -p aterm-grid --test conformance_offload
cargo build --locked -p aterm
cargo run --quiet --locked -p aterm -- --version
```

The final command should report `[workspace.package] version` from the root
`Cargo.toml`. Always build from the workspace — the individual crates are not
published to crates.io and their APIs are not stable yet; see
[README ▸ Build from source](README.md#build-from-source) for why
`cargo install aterm` is the wrong move.

The private development line builds on the Trust toolchain instead. Every
derived-model obligation is discharged in-process, so a stock clone verifies
for real. Where the Trust tools add an analysis the in-process checker cannot
express, that analysis is skipped on machines without them: the test still
reports `ok`, and the reason is printed to stderr — run
`cargo test -- --nocapture` to see it. Nothing here requires those tools to go
green.

### If `cargo` says `error: toolchain 'trust' is not installed`

That is rustup speaking, and it means the private line's `trust` toolchain link
(`~/.rustup/toolchains/trust`) no longer reaches the atpkg-managed store that
holds the compiler. Run `aterm pkg doctor`: it names the seam that broke, and
`aterm pkg doctor --fix` re-points the link at `store/trust/current`. Do not
rebuild a toolchain from source to answer that message, and do not add a
`cargo`/`rustc`/`rustup` shim — the managed `bin/` never carries one by design.
On this public snapshot the message cannot occur: it pins a stock toolchain.

Run the focused tests for every crate you change; they are expected to pass on
a fresh clone of this tree.

There is no hosted CI: nothing runs automatically on a pull request, so paste
the output of the tests you ran into the description.

**Which gate is the contract, and which one is yours.** The gate that decides
whether a change lands is `tools/verify.sh --fast` — a nineteen-stage local
ladder (`crates/aterm-verify`) that a maintainer runs on the rebased branch, on
the development line, at land time. You are not expected to run it, and this
file does not ask you to: that ladder drives the development line's own
toolchain, while this snapshot deliberately pins a stock Rust release (see
[PUBLICATION.md](PUBLICATION.md)). This paragraph used to call
`cargo run -q -p xtask -- gate <check>` "the local gate ladder the project uses
in place of CI", which read as though the verb you can run here were the
contract. It is not, and being plain about that is worth more than the
symmetry.

What you *can* run on this snapshot, all on the pinned toolchain:

* `cargo test --locked` for every crate you touched. This is the one that
  matters, and the expectation is that it is green on a fresh clone.
* `cargo run -q -p xtask -- gate <check>` for the source-walk lanes. These
  shell out to nothing — they are in-process walks of the checked-in tree —
  so they run anywhere the workspace builds: `drift`, `dormant`, `mainloop`,
  `lockorder`, `wasmloop`, `scope`, `lazyinit`, `fault` and `counts`. Run the
  ones touching your change. A bare `gate` prints the list of checks and fails.

**`gate all` is not that ladder**, and a red `gate all` here is not evidence
about your change. `crates/xtask/src/gate.rs` marks the verb MANUAL ONLY —
nothing invokes it automatically — and its roster includes `lint`, whose tippy
and trustfmt lanes drive the development line's own toolchain and whose guard
lane shells out to a set of `tools/*.sh` scripts (`paint_guard`, `spin_guard`
and friends) that [PUBLICATION.md](PUBLICATION.md)'s export list does not name;
the only file it names under `tools/` is the installer. A lane that could not
run is reported as reaching *no verdict*, and `gate lint` — and so `gate all` —
returns failure for it rather than a pass, deliberately: a check that did not
run must never read as a check that passed. Run the specific lanes above
instead.

If a change affects how the window looks or feels, also run a real aterm
instance, capture the rendered frame through `aterm ctl image`, and include
before/after evidence.

## Issues and pull requests

Use [GitHub Issues](https://github.com/alabsystems/aterm/issues) for public bug
reports, focused feature discussion, and reproducible build problems. Keep pull
requests small enough to review and explain the user-visible behavior they
change.

Because the public tree is an export, a merged change lands in the private line
first and reaches this repository with the next snapshot rather than as a
commit on top of your pull request.

Do not attach credentials, sensitive terminal logs, private preview artifacts,
or internal ALab material. Suspected vulnerabilities must follow
[SECURITY.md](SECURITY.md), not public issues.

This is a best-effort project with no response-time guarantee.

## Licensing

Unless you conspicuously state otherwise, a contribution intentionally
submitted for inclusion is licensed under Apache License 2.0 on the same
inbound-and-outbound terms as the project. Contributions to files or components
already marked MIT are submitted under MIT.
