<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Contributing

This repository is a public source snapshot of aterm's private development
line, cut at release time. Focused fixes and well-scoped improvements are
welcome. It carries the whole Rust workspace;
[PUBLICATION.md](PUBLICATION.md) lists what the export leaves out.

## Build and test

The snapshot pins the Trust toolchain (`rust-toolchain.toml`), which atpkg
installs on Apple-silicon macOS
([README ▸ Build from source](README.md#build-from-source); `aterm help rust`
explains the toolchain). Every command names its lane: `targo --unverified
<verb>` builds with no proof claim, `targo trust <verb>` verifies. From the
workspace root:

```sh
targo --unverified check --locked -p aterm
targo --unverified test --locked -p aterm-grid --test conformance_offload
targo --unverified build --locked -p aterm
targo --unverified run --quiet --locked -p aterm -- --version
```

The last command prints the root `Cargo.toml`'s `[workspace.package] version`.
Build from the workspace: the crates are not on crates.io (the crates.io
`aterm` is an unrelated project) and their APIs are not stable.

Derived-model obligations run in-process, so a clone verifies for real: plain
tests check every scalar model on the interpreter and run neither `ty` nor
`ay`. A function-valued model, which only `ty` can walk, ends `NotRun` there,
never a pass. `verify spec` and `verify checkers` (both in `--release`) require
`ty` and `ay` and fail closed without them; ALab's default set provides both
(`aterm pkg install --default-set`).

**`'rustc' is not installed for the custom toolchain 'trust'`**: rustup's
`trust` link no longer reaches the atpkg store. Run `aterm pkg repair`; never
rebuild a toolchain from source for it. **`toolchain 'trust' is not
installed`** with no `targo` either: run `aterm pkg install trust` (install
aterm first if it is missing).

What to run before a pull request:

* `targo --unverified test --locked -p <crate>` for every crate you touched,
  expected green on a fresh clone once `targo --unverified verify provision`
  has run; a COULD NOT RUN names what provides the tool.
* `targo --unverified run -q -p aterm-verify -- fmt`, the formatter check
  (`fmt --fix` rewrites what it finds).
* `targo-tippy --workspace --all-targets --keep-going -- -D warnings`,
  zero warnings.
* `targo --unverified run -q -p aterm-census -- --all`, the source-walk
  censuses and the source guards (`--help` lists them).

The development line runs these and the rest of the checks as
`targo --unverified verify` (`crates/aterm-verify`, an alias in the
workspace's `.cargo/config.toml`), and every release runs its `release` set
before anything is published. A check that needs the
development line's machinery reports NOT RUN or COULD NOT RUN here; that says
nothing about your change.

There is no hosted CI: paste the output of the tests you ran into the pull
request. For a change to how the window looks or feels, capture the frame with
`aterm ctl image` on a real aterm and include before and after.

## Issues and pull requests

Use [GitHub Issues](https://github.com/alabsystems/aterm/issues) for bug
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
