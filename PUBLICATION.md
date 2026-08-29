<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Public source snapshot

This repository is the public source snapshot of aterm, and it carries two
different things:

- **The source snapshot** — the tree you are reading. Each release adds one
  commit here: a squashed export of the private development repository, cut at
  release time. Every public release has a source snapshot beside it, and the
  release cutter refuses to cut until this tree already carries the release's
  version. It contains source, not prebuilt binaries.
- **The releases** — the macOS application, published on this repository's
  [Releases](https://github.com/alabsystems/aterm/releases) page as a signed,
  notarized DMG, together with the signed update manifest and machine roster
  that installed copies verify. The same page also hosts the signed
  `atpkg-index-N` package index that `aterm pkg` reads.

When a release lands here, [alab.systems](https://alab.systems) follows it
automatically: `/terminal` is rebuilt from the promoted commit, the download
button points at the current release asset, and `/releases` carries the notes.

Both report the same version: a single `MAJOR.MINOR.0` whose patch slot is
always `0` and whose `MINOR` is the knob that moves, described in
[VERSIONING.md](VERSIONING.md). The authoritative value is the root Cargo
workspace version, which `aterm --version` prints and which the staging gate
checks by building a fresh, credential-free clone. Historical private labels
are not public releases.

## What the transform changes

The publication transform makes only reviewable boundary changes:

- sets the public Cargo workspace and first-party lockfile records to the
  public `X.Y.0`;
- pins the public build to a stock Rust release — `rust-toolchain.toml` names
  it — and omits the private Trust-only Cargo configuration;
- points repository and update defaults at the public `alabsystems` namespace;
- normalizes local-machine path and credential-shaped test fixtures without
  changing the behavior they test; and
- excludes everything outside the export allowlist — operational notes, the
  changelog and release ledger, man pages, the release and publication
  machinery, internal proof packets, generated tool caches, and unused traced
  art inputs whose source rights were not established.

## What the export contains

The export is an allowlist, not a denylist: only listed paths ship. What ships
is the full Rust workspace — including developer and release-helper source, so
the reviewed `Cargo.lock` needs no regeneration — plus modified vendored
crates, the public README media, the installer script `tools/install.sh`, and
the license material needed for that boundary.

Where a test's only fixture is something the export omits — the `ay`
proof bundles, the changelog — that test is left out too, so a fresh clone of
this tree tests clean. Those obligations still run on the development line,
where their inputs exist.

It contains no release credentials, no prebuilt executable, no updater payload,
and no managed ALab tool package. Those are distributed, not exported: the
application arrives as a release artifact and the toolchain through
`aterm pkg`, each verified against the signing chain described in
[README ▸ Security model](README.md#security-model). That chain's trust anchor
is a committed public-key constant in this source tree — reviewable here, never
supplied by an environment variable.

Some compatibility and regression tests retain historical strings from the
retired two-component update-channel scheme, whose archive lives only in the
private origin. Those strings are fixtures, not public version claims, and the
updater never selects such a tag.

See [NOTICE](NOTICE) for third-party attribution and [SECURITY.md](SECURITY.md)
for private vulnerability reporting.
