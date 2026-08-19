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
`cargo test -- --nocapture` to see it. A few certificate lanes instead fail
outright without those tools and are not expected to pass on a stock clone.

Run the focused tests for every crate you change. A few test targets ship here
but read internal proof packets and the changelog that the export omits — the
`ay` certificate and bundle-hygiene tests in `aterm-spec`, and the changelog
test in `aterm-release`; they fail on this tree and are not part of the
contributor gate.

There is no hosted CI: nothing runs automatically on a pull request, so paste
the output of the tests you ran into the description.
`cargo run -q -p xtask -- gate` is the local gate ladder the project uses in
place of CI; run at least the lanes touching your change. If a change affects
how the window looks or feels, also run a real aterm instance, capture the
rendered frame through `aterm ctl image`, and include before/after evidence.

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
