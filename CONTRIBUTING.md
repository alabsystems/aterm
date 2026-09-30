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

The snapshot pins the Trust toolchain in `rust-toolchain.toml`, as the private
development line does. Install aterm ([README ▸ Install](README.md#install));
atpkg installs Trust (`aterm pkg install trust` does it by hand, and
`aterm help rust` explains the toolchain). atpkg ships Trust for Apple-silicon
macOS today; elsewhere, build Trust from
[source](https://github.com/alabsystems/trust). On macOS you also need the Xcode
Command Line Tools; Linux and Windows build and test from source too, though
only macOS has released binaries, an installer, and the self-updater. Every
command names its lane — `targo --unverified <verb>` builds with no proof
claim, `targo trust <verb>` verifies. From the workspace root:

```sh
targo --unverified check --locked -p aterm
targo --unverified test --locked -p aterm-grid --test conformance_offload
targo --unverified build --locked -p aterm
targo --unverified run --quiet --locked -p aterm -- --version
```

The final command should report `[workspace.package] version` from the root
`Cargo.toml`. Always build from the workspace — the individual crates are not
published to crates.io and their APIs are not stable yet; see
[README ▸ Build from source](README.md#build-from-source) for why
`cargo install aterm` is the wrong move.

Every derived-model obligation is discharged in-process, so a clone verifies
for real. Where the other ALab tools (`ty`, `ay`) add an analysis the
in-process checker cannot express, that analysis is skipped on machines
without them: the test still reports `ok`, and the reason is printed to
stderr — run `targo --unverified test -- --nocapture` to see it. Nothing here
requires those tools to go green.

### If `cargo` says `error: toolchain 'trust' is not installed`

That is rustup speaking, and it means the `trust` toolchain link
(`~/.rustup/toolchains/trust`) no longer reaches the atpkg-managed store that
holds the compiler. Run `aterm pkg doctor`: it names the seam that broke, and
`aterm pkg doctor --fix` re-points the link at `store/trust/current`. Do not
rebuild a toolchain from source to answer that message, and do not add a
`cargo`/`rustc`/`rustup` shim — the managed `bin/` never carries one by design.
The one place those names exist as files aterm lays is the session-scoped
reroute directory (`<prefix>/reroute`, `docs/DESIGN-toolchain-reroute-2026-09-07.md`),
which only an aterm session puts first on its own PATH — and it is not `bin/`.
With no aterm installed at all, `aterm pkg install trust` is not available yet:
install aterm first.

Run the focused tests for every crate you change; they are expected to pass on
a fresh clone of this tree.

There is no hosted CI: nothing runs automatically on a pull request, so paste
the output of the tests you ran into the description.

**Which gate is the contract, and which one is yours.** The gate that decides
whether a change lands is `tools/verify.sh` — a local ladder of stages
(`crates/aterm-verify`) that a maintainer runs on the rebased branch, on
the development line, at land time. You are not expected to run it, and this
file does not ask you to: that ladder drives the development line's own
toolchain and private configuration, which this snapshot does not carry (see
[PUBLICATION.md](PUBLICATION.md)). This paragraph used to call
`cargo run -q -p xtask -- gate <check>` "the local gate ladder the project uses
in place of CI", which read as though the verb you can run here were the
contract. It is not, and being plain about that is worth more than the
symmetry.

What you *can* run on this snapshot, all on the pinned toolchain:

* `targo --unverified test --locked` for every crate you touched. This is the one that
  matters, and the expectation is that it is green on a fresh clone.
* `targo --unverified run -q -p aterm-census -- . [--mainloop|--locks|--wasm|--scope|--lazy-init]`
  for the source-walk censuses: main-loop reach, the lock-order graph, the
  wasm process, scope cardinality and lazy-init reentrancy. They shell out to
  nothing — they are in-process walks of the checked-in tree — so they run
  anywhere the workspace builds; with no flag it runs the main-loop and
  lock-order pair, and `--help` prints the list. Run the ones touching your
  change.

**`xtask gate` is not that ladder.** Its verbs are checks the development
line's ladder shells into, and they drive that line's own
toolchain: `gate lint`'s trustfmt passes and the cross-cell type-checks among
them. A check that could not run is reported as reaching *no verdict* and
returns failure rather than a pass, deliberately: a check that did not run must
never read as a check that passed. So a red `xtask gate` verb here is not
evidence about your change; the tests and censuses above are.

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
