<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Versioning

aterm has **one** version: `[workspace.package] version` in the root
`Cargo.toml`. Read it there — never recall it, never infer it from a tag, a
release listing, or a file name.

```
public release   MAJOR.MINOR.0
dev build        MAJOR.MINOR.0+g<short-sha>
```

That one number is what the macOS application reports in **About aterm** and
`aterm --version`, what the release tag `vMAJOR.MINOR.0` names, what the update
manifest carries, what the DMG is called, and what the public source snapshot
cut beside that release declares. The tag, the manifest `version`, and the
`aterm-<version>.dmg` asset name are bound to each other: an installed copy
refuses a release whose three spellings disagree.

- **The patch slot is always `0`.** `MINOR` is the knob meant for constant
  motion; public tags are always `vMAJOR.MINOR.0`. There are no patch releases,
  so a fix — security or otherwise — ships as the next `MINOR`.
- **The commit sha rides in SemVer build metadata (`+g…`), never the patch
  slot.** `0.1.0+g9d1ce1d2` satisfies `version = "0.1"`, `"0.1.0"` and
  `"=0.1.0"`; a pre-release (`-dev.N+g…`) satisfies none of them, and a hash in
  the patch slot makes versions move *backwards*, so Cargo would read later
  builds as downgrades.

## How an installed copy picks a release

The updater decides in two stages, and only the second uses the build number.

1. **Selection.** The client walks the channel's non-draft releases that carry
   an update manifest, keeps only canonical three-component
   `vMAJOR.MINOR.PATCH` tags, and takes the greatest numeric
   `(MAJOR, MINOR, PATCH)`. The winner's spelling is re-derived from its parsed
   numbers, so `v01.2.3` can never stand in for `v1.2.3`. Anything malformed —
   non-numeric, empty or leading-zero components, more than three components —
   is a hard error rather than a silent narrowing, and the retired
   two-component scheme below is skipped outright.
2. **Apply.** The selected manifest's `build_number` must strictly exceed the
   running build. Every build carries a monotonic build number
   (`CFBundleVersion`); this is the downgrade gate.

Because selection is numeric, a published version must **increase** on every
cut: a reused or lowered version is never elected at all, and every installed
copy would go on reporting that it is up to date.

## The retired two-component scheme

The macOS update channel once ran its own two-component `MAJOR.MINOR` lineage
that Cargo's version played no part in. That lineage is retired. Those releases
remain in the private origin's archive, but they are no longer installable and
no longer authority: the client classifies a two-component tag as legacy and
skips it, and the publisher does the same. There is deliberately **no backwards
compatibility** — a copy installed from a pre-migration release will never
select a new one and must be reinstalled once (see the installer in
[README ▸ Install](README.md#install)).

Some compatibility and regression tests still contain historical strings from
that scheme. Those are fixtures, not version claims.

## The public source snapshot

The publication transform normalizes the public workspace and every first-party
`Cargo.lock` record to the public `MAJOR.MINOR.0` and tags the snapshot to match
the application. First-party crates are detected structurally (a `[[package]]`
with no `source` line in `Cargo.lock`), so the normalization is exact regardless
of which versions third-party crates happen to use. See
[PUBLICATION.md](PUBLICATION.md) for the rest of that boundary.
