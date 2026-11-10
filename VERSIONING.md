<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Versioning

aterm has one version: `[workspace.package] version` in the root `Cargo.toml`.
Read it there, not from a tag, a release listing or a file name.

```
historical public release   MAJOR.MINOR.0
historical dev build        MAJOR.MINOR.0+g<short-sha>
```

That number is what **About aterm** and `aterm --version` report, what the
release tag `v<version>` names, what the update manifest carries, what the
DMG is called, and what the public source snapshot cut beside that release
declares. The tag, the manifest `version` and the `aterm-<version>.dmg` asset
name are bound to each other: an installed copy refuses a release whose three
spellings disagree.

- **The historical patch slot is `0`.** On that line a fix,
  security or otherwise, ships as the next `MINOR`.
- **The commit sha rides in SemVer build metadata (`+g…`), never the patch slot
  or a pre-release.** `0.1.0+g9d1ce1d2` still satisfies `version = "0.1"`,
  `"0.1.0"` and `"=0.1.0"`; a pre-release (`-dev.N+g…`) satisfies none of them,
  and a hash in the patch slot would make later builds read as downgrades.

## Calendar product versions (staged migration)

The forward product form is `1.YYYYMMDD.N`: the UTC date when the source version
is reserved, and a positive counter for that product's cuts on that day. Repeated
cuts increment `N`; a later UTC day starts at `1`. Real Gregorian dates are
required, and date-shaped versions with `N = 0` are refused. The daily counter
is bounded by the existing clients' nine-digit component limit. A regressed UTC
clock or exhausted counter refuses allocation instead of issuing an older version.

The shared product-version reader accepts this form alongside historical releases.
The Aterm cutter compares all three components. Its `--bump` keeps the historical
minor policy until the committed workspace version adopts calendar form; calendar
sources then allocate against the run's fixed UTC selection time. The existing
source Main CAS, claim and journal retain that identity. Resuming a cut, including
after midnight, reuses its version and counter. No old tag, artifact, build id or
journal is rewritten.

This is reader/allocation support, not activation of new public labels. The
publication engine's version/bump/doctor policy and required consumers must be
qualified together before the first calendar source claim. Harness source stamps
and Codex snapshot count/source/upstream bindings remain required; their source
version generators still need coordinated migration. Compiler and upstream
technical versions remain separate. No build number is interpreted as a date.

## How an installed copy picks a release

The updater decides in two stages, and only the second uses the build number.

1. **Selection.** The client walks the channel's non-draft releases that carry
   an update manifest, keeps only canonical `vMAJOR.MINOR.PATCH` tags, and takes
   the greatest numeric `(MAJOR, MINOR, PATCH)`. The winner's spelling is
   re-derived from its parsed numbers, so `v01.2.3` can never stand in for
   `v1.2.3`. A malformed tag (non-numeric, empty or leading-zero components,
   more than three components) is a hard error, not a silent narrowing.
   Two-component tags (`v0.25`…`v0.61`) are a retired lineage that client and
   publisher both skip; a copy installed from one never selects a newer release
   and must be reinstalled once ([README ▸ Install](README.md#install)).
2. **Apply.** The selected manifest's `build_number` must strictly exceed the
   running build's. Every build carries a monotonic build number
   (`CFBundleVersion`); this is the downgrade gate.

Because selection is numeric, a published version must increase on every cut:
a reused or lowered version is never selected, and every installed copy would
go on reporting that it is up to date.

## The public source snapshot

The public source snapshot carries the workspace version exactly as committed
(`Cargo.toml` and every `Cargo.lock` record) and is tagged `v<version>` to
match the application. Nothing rewrites it on the way out, so the publication
engine's historical policy refuses a nonzero patch until the coordinated calendar
migration is activated. See
[PUBLICATION.md](PUBLICATION.md) for the rest of that boundary.
