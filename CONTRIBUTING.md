<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Contributing

aterm `v0.1.0` is the first public product-source snapshot. Focused fixes and
well-scoped improvements are welcome. The public tree includes the Rust
workspace but intentionally omits private operational notes, release
credentials, internal proof packets, binaries, installers, and managed ALab
packages.

## Build and test

The repository pins its tested stock Rust toolchain. From the workspace root:

```sh
cargo check --locked -p aterm
cargo test --locked -p aterm-grid --test conformance_offload
cargo build --locked -p aterm
./target/debug/aterm --version
```

The final command should report `aterm 0.7.0`. Build from the workspace; the
individual crates are not published to crates.io and their APIs are not stable
yet.

Run the focused tests for every crate you change. If a change affects how the
window looks or feels, also run a real aterm instance, capture the rendered
frame through `aterm ctl image`, and include before/after evidence in the pull
request.

## Issues and pull requests

Use [GitHub Issues](https://github.com/alabsystems/aterm/issues) for public bug
reports, focused feature discussion, and reproducible build problems. Keep pull
requests small enough to review and explain the user-visible behavior they
change.

Do not attach credentials, sensitive terminal logs, private preview artifacts,
or internal ALab material. Suspected vulnerabilities must follow
[SECURITY.md](SECURITY.md), not public issues.

This is a best-effort project with no response-time guarantee.

## Licensing

Unless you conspicuously state otherwise, a contribution intentionally
submitted for inclusion is licensed under Apache License 2.0 on the same
inbound-and-outbound terms as the project. Contributions to files or components
already marked MIT are submitted under MIT.
