<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Public source snapshot

aterm's public version series began at `v0.1.0`; this snapshot is `v0.5.0`. Each
one is produced as an immutable, single-commit source snapshot from the private
development repository. aterm carries a single `MAJOR.MINOR.0` version (see
`VERSIONING.md`): the patch slot is always `0` and `MINOR` is the knob that
moves, and the macOS application, its release tag, and this source snapshot all report
it. Historical private labels are not public releases.

The publication transform makes only reviewable boundary changes:

- sets the public Cargo workspace and first-party lockfile records to the public
  `X.Y.0` — `0.5.0` for this snapshot;
- pins the public build to stock Rust `1.97.1` and omits private Trust-only
  Cargo configuration;
- points repository and update defaults at the public `alabsystems` namespace;
- normalizes local-machine path and credential-shaped test fixtures without
  changing the behavior they test; and
- excludes operational notes, internal proof write-ups, generated tool caches,
  and unused traced art inputs.

The exported source contains the full Rust workspace—including developer and
release-helper source for Cargo lockfile closure—modified vendored crates,
public media, and the license material needed for that boundary. It contains
no release credentials, prebuilt executable, installer, public updater payload,
or managed ALab tool package.

Some compatibility and regression tests retain historical strings such as
`v0.56` from the retired two-component update-channel scheme. Those strings are
fixtures, not public version claims. The authoritative public package version is
the root Cargo workspace version and `aterm --version`, both checked as `0.5.0`
during staging.

See [NOTICE](NOTICE) for third-party attribution and [SECURITY.md](SECURITY.md)
for private vulnerability reporting.
