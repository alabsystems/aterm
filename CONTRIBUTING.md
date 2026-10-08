<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Contributing

This repository is a public source snapshot of aterm's private development
line, cut at release time. Focused fixes and well-scoped improvements are
welcome. The public tree includes the whole Rust workspace;
[PUBLICATION.md](PUBLICATION.md) lists what the export leaves out — release
credentials, prebuilt binaries, internal operational notes and proof packets.

## Build and test

The snapshot pins the Trust toolchain in `rust-toolchain.toml`, as the
development line does. Install aterm ([README ▸ Install](README.md#install))
and atpkg installs Trust (`aterm pkg install trust` by hand; `aterm help rust`
explains the toolchain). atpkg ships Trust for Apple-silicon macOS only, so the
snapshot builds and tests there, with the Xcode Command Line Tools installed.
Every command names its lane — `targo --unverified <verb>` builds with no proof
claim, `targo trust <verb>` verifies. From the workspace root:

```sh
targo --unverified check --locked -p aterm
targo --unverified test --locked -p aterm-grid --test conformance_offload
targo --unverified build --locked -p aterm
targo --unverified run --quiet --locked -p aterm -- --version
```

The last command reports `[workspace.package] version` from the root
`Cargo.toml`. Build from the workspace: the crates are not published to
crates.io and their APIs are not stable
([README ▸ Build from source](README.md#build-from-source) says why
`cargo install aterm` is the wrong move).

Every derived-model obligation is discharged in-process, so a clone verifies
for real. Where `ty` or `ay` adds an analysis the in-process checker cannot
express, it is skipped on machines without them: the test reports `ok` and
prints the reason to stderr (`targo --unverified test -- --nocapture`).

**`'rustc' is not installed for the custom toolchain 'trust'`** means rustup's
`trust` link no longer reaches the atpkg store; `targo` still works. Run `aterm
pkg repair`. Never rebuild a toolchain from source for it. **`toolchain 'trust'
is not installed`** with no `targo` either means Trust is not installed yet: run
`aterm pkg install trust`. With no aterm installed, install aterm first.

What you can run on this snapshot, on the pinned toolchain:

* `targo --unverified test --locked -p <crate>` for every crate you touched —
  the one that matters, expected green on a fresh clone.
* `targo --unverified run -p xtask -- gate lint --fmt-only`, the formatter check.
* `targo-tippy --workspace --all-targets --keep-going -- -D warnings`, zero
  warnings.
* `targo --unverified run -q -p aterm-census -- . [--mainloop|--locks|--wasm|--scope|--lazy-init]`,
  the in-process source-walk censuses (no flag runs main-loop and lock order;
  `--help` lists them). Run the ones touching your change.

The gate that decides whether a change lands is `tools/verify.sh`
(`crates/aterm-verify`), which a maintainer runs on the development line at
land time; it drives that line's toolchain and private configuration, which
this snapshot does not carry, so you are not asked to run it. Other `xtask gate`
verbs need the development line's machinery; one that cannot run here prints
NOT RUN or COULD NOT RUN, which says nothing about your change.

There is no hosted CI: paste the output of the tests you ran into the pull
request. If a change affects how the window looks or feels, run a real aterm,
capture the frame with `aterm ctl image`, and include before/after evidence.

## Issues and pull requests

Use [GitHub Issues](https://github.com/alabsystems/aterm/issues) for public bug
reports, focused feature discussion and reproducible build problems. Keep pull
requests small and explain the user-visible behavior they change. A merged
change lands in the private line first and reaches this repository with the
next snapshot, not as a commit on top of your pull request.

Do not attach credentials, sensitive terminal logs, private preview artifacts
or internal ALab material. Suspected vulnerabilities follow
[SECURITY.md](SECURITY.md), not public issues. This is a best-effort project
with no response-time guarantee.

## Licensing

Unless you conspicuously state otherwise, a contribution intentionally
submitted for inclusion is licensed under Apache License 2.0 on the same
inbound-and-outbound terms as the project. Contributions to files or components
already marked MIT are submitted under MIT.
