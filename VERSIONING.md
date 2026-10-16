<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Versioning

aterm has one version: `[workspace.package] version` in the root `Cargo.toml`.
Read it there, not from a tag, a release listing or a file name.

```
public release   MAJOR.MINOR.0
dev build        MAJOR.MINOR.0+g<short-sha>
```

That number is what **About aterm** and `aterm --version` report, what the
release tag `vMAJOR.MINOR.0` names, what the update manifest carries, what the
DMG is called, and what the public source snapshot cut beside that release
declares. The tag, the manifest `version` and the `aterm-<version>.dmg` asset
name are bound to each other: an installed copy refuses a release whose three
spellings disagree.

- **The patch slot is always `0`.** There are no patch releases; a fix,
  security or otherwise, ships as the next `MINOR`.
- **The commit sha rides in SemVer build metadata (`+g…`), never the patch slot
  or a pre-release.** `0.1.0+g9d1ce1d2` still satisfies `version = "0.1"`,
  `"0.1.0"` and `"=0.1.0"`; a pre-release (`-dev.N+g…`) satisfies none of them,
  and a hash in the patch slot would make later builds read as downgrades.

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
(`Cargo.toml` and every `Cargo.lock` record) and is tagged `vMAJOR.MINOR.0` to
match the application. Nothing rewrites it on the way out, so the publication
engine refuses to promote a tree whose patch slot is not `0`. See
[PUBLICATION.md](PUBLICATION.md) for the rest of that boundary.
