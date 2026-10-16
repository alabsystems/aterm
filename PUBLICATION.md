<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Public source snapshot

This repository is the public source snapshot of aterm, and it carries two
different things:

- **The source snapshot** — the tree you are reading. Each release adds one
  commit here: a squashed export of the private development repository, cut at
  release time. The release cutter refuses to cut until this tree carries the
  release's version. It contains source, not prebuilt binaries.
- **The releases** — the macOS application, published on this repository's
  [Releases](https://github.com/alabsystems/aterm/releases) page as a signed,
  notarized DMG, together with the signed update manifest and machine roster
  that installed copies verify. The same page hosts the signed `atpkg-index-N`
  package index that `aterm pkg` reads.

When a release lands here, [alab.systems](https://alab.systems) follows it:
`/terminal` is rebuilt from the public commit, the download button points at
the current release asset, and `/releases` carries the notes.

Both report the same version, a single `MAJOR.MINOR.0`
([VERSIONING.md](VERSIONING.md)): the root Cargo workspace version, exported
exactly as committed, which `aterm --version` prints and which the staging gate
checks by building a fresh, credential-free clone.

## What the transform changes

The publication transform makes only reviewable boundary changes:

- keeps the Trust toolchain pin (`rust-toolchain.toml`; a public user installs
  aterm, and atpkg installs Trust) and omits the private Cargo configuration;
- writes the `vendor/astream` submodule, which names a private repository, as
  plain files: the crates the build reaches;
- drops build-target tables whose files the export withholds;
- points repository and update defaults at the public `alabsystems` namespace;
  and
- anonymizes local-machine paths in test fixtures without changing the
  behavior they test.

## What the export contains

The export is an allowlist: only listed paths ship. That is the full Rust
workspace — including developer and release-helper source, so the reviewed
`Cargo.lock` needs no regeneration — plus the modified vendored crates, the
public README media, the installer script `tools/install.sh`, the community
theme kit `contrib/themes`, and the license material.

Everything else stays private: operational notes, the changelog and release
ledger, man pages, the release and publication machinery, internal proof
packets, generated tool caches, and unused traced art inputs whose source
rights were not established. A test whose only fixture is something the export
omits (the `ay` proof bundles, the changelog) is left out too, so a fresh clone
tests clean; those obligations run on the development line.

It contains no release credentials, no prebuilt executable, no updater payload,
and no managed ALab tool package. Those are distributed, not exported: the
application arrives as a release artifact and the toolchain through
`aterm pkg`, each verified against the signing chain described in
[README ▸ Security model](README.md#security-model). That chain's trust anchor
is a committed public-key constant in this source tree, never supplied by an
environment variable.

Some compatibility and regression tests carry version strings from the retired
two-component update-channel scheme. Those strings are fixtures, not public
releases, and the updater never selects such a tag.

See [NOTICE](NOTICE) for third-party attribution and [SECURITY.md](SECURITY.md)
for private vulnerability reporting.
