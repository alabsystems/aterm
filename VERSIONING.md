# Versioning

aterm has **one** version: `[workspace.package] version` in the root
`Cargo.toml`. The scheme is the constellation-wide one managed by the
`publication` repo — see the canonical `publication/VERSIONING.md`.

```
public release   MAJOR.MINOR.0
dev build        MAJOR.MINOR.0+g<short-sha>
```

- **The patch slot is always `0`** (`0.5.0` today). Bump `MINOR` freely — that
  is the knob meant for constant motion. Public tags are always `vMAJOR.MINOR.0`.
- **The commit sha rides in SemVer build metadata (`+g…`), never the patch
  slot.** `0.1.0+g9d1ce1d2` satisfies `version = "0.1"`, `"0.1.0"` and
  `"=0.1.0"`; a pre-release (`-dev.N+g…`) satisfies none of them, and a hash in
  the patch slot makes versions move *backwards* so Cargo reads later builds as
  downgrades.
- **Never mint a version from a dirty tree** — `pub version` appends `.dirty`,
  which is not publishable.

`0.5.0` is what the public source snapshot carries, what the macOS app reports
in About and `aterm --version`, the release tag `v0.5.0`, the manifest
`version`, and the DMG name `aterm-0.5.0.dmg`.

> **The lineage is the PUBLIC one, and it is authoritative.** aterm's public
> series began at `v0.1.0` (`public/v0.1.0`) and that is the number the project
> counts from — the internal `0.62` lineage is retired and must not be
> reintroduced. The `public/` prefix (`DEV_TAG_PREFIX_DEFAULT` in
> `publish/config.sh`) covers ONLY the source-snapshot provenance tag.
> `cargo ship cut` tags the app release **bare** `vX.Y.0`
> (`gates::tag_free` → `format!("v{version}")`), which DOES share a namespace
> with the retired private tags — see the warning below.

`cargo ship cut` derives the release version from `[workspace.package] version`
by resetting DEV to `0` (`publish::release_version_from_workspace`). It never
rewrites `Cargo.toml` or `Cargo.lock`; it hands the derived version to both
architecture builds as `ATERM_APP_RELEASE_VERSION`, and every shipped CLI, GUI,
diagnostics, protocol, plist, provenance, and manifest identity is checked
against that single value.

## Cutting the next release

Bump the MINOR **first**, then cut — the patch slot stays `0`:

```sh
pub bump --minor --commit   # 0.5.0 -> 0.6.0
```

Cutting twice without a bump is refused: `verify::derive_cut_mode` sees `v0.5.0`
already published and tells the operator to bump `[workspace.package] version`,
naming the next release (`v0.6.0`). MAJOR moves the same way.

The workspace sits at `0.5.0`, so the next cut publishes `v0.5.0` with no bump at
all; the bump above is what the cut *after* that one needs. Run
`pub version --release` rather than trusting this paragraph — it prints what
`alabsystems` would actually tag.

> **Why the lineage resumes at `0.5.0` and not `0.2.0`.** `gates::tag_free` mints
> a **bare** `v{version}` tag on the private origin, and that namespace already
> holds real historical tags: `v0.1.0`, `v0.3.0`, `v0.4.0`, `v0.4.1` and
> `v0.5.9`..`v0.5.14`. Of the `v0.N.0` spellings the cutter actually mints, only
> `0.1`, `0.3` and `0.4` are consumed — so `0.5.0` is the lowest MINOR from which
> **every** subsequent bump (`0.6.0`, `0.7.0`, …) is collision-free, permanently.
> The archive's `v0.15.*`..`v0.21.*` tags are three-component *timestamps*, never
> `.0`, so they never contest a `vN.N.0` mint.
>
> This is a bounded, evidence-checked skip inside the public lineage — not a
> restart. Verify before any bump rather than trusting this list:
>
> ```sh
> git ls-remote --tags origin 'refs/tags/v*.*.0'
> ```
>
> The durable fix is the `public/` prefix (`DEV_TAG_PREFIX_DEFAULT` in
> `publish/config.sh`), which gives publication its own namespace. It is not wired
> into `gates::tag_free`/`ledger` yet: the publisher re-plays the client's election
> against the private repo (`verify::scan_release_page`), and that replay only
> admits canonical `vX.Y.Z`, so prefixing the mint needs the replay moved with it.

## Public source snapshot

`publish/transforms.sh` (driven by the publication engine) normalizes the public
workspace and every first-party `Cargo.lock` record from the internal `X.Y.DEV`
down to the public `X.Y.0` and tags the snapshot `vX.Y.0` — the same number the
app carries. First-party crates are detected structurally (a `[[package]]` with
no `source` line in `Cargo.lock`), so the normalization is exact regardless of
which versions third-party crates happen to use. This matches the
constellation-wide scheme used across the ALab repositories.

## The private app/update channel

The macOS update channel used to run its own two-component `MAJOR.MINOR`
lineage (`v0.25` … `v0.61`) that Cargo's version played no part in. **That
lineage was retired in this change.** Those releases stay published in the
GitHub archive, but they are no longer installable and are no longer authority:
the client classifies a two-component tag as `TagKind::Legacy` and skips it
(`parse_numeric_tag` / `select_authoritative_release` in
`crates/aterm-update/src/github.rs`), and the publisher does the same
(`publish::parse_release_tag`). There is deliberately **no backwards
compatibility** — an app installed from a pre-migration release will never
select a new one and must be reinstalled once (`tools/install.sh`).

**Two keys, not one.** The updater decides in two stages, and only the second is
the build number:

1. **Selection** — the client walks the channel's non-draft releases carrying an
   `aterm-appcast.toml`, keeps only canonical three-component
   `vMAJOR.MINOR.PATCH` tags, and takes the greatest numeric
   `(MAJOR, MINOR, PATCH)`. The winner's spelling is re-derived from its parsed
   numbers (`canonical_authority_version`), so `v01.2.3` can never stand in for
   `v1.2.3`. The manifest's `version` and the `aterm-<version>.dmg` asset name
   must both equal that tag's version. Retired two-component tags are skipped;
   anything else malformed (non-numeric, empty or leading-zero components, more
   than three components) is a hard error, not a silent narrowing.
2. **Apply** — the selected manifest's `build_number` must strictly exceed the
   running build. This is the downgrade gate, unchanged by the migration.

Because selection is numeric, the published version must **increase** on every
cut — bump MINOR, never reuse or lower it.

`RELEASES.ledger` still mints and claims the build numbers
(`n = max(tail + 1, unix_now)`, a compare-and-swap on the tail; see
`docs/RELEASING.md`). It is no longer a version lineage: column 2 now records
the unified release version, and a cut never reads it to decide what to publish.
