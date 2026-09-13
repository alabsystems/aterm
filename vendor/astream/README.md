<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# vendor/astream — the bus crates `aterm-link` builds against

Upstream: **github.com/alabsystems/astream**, branch `main`, commit `bb98d610894afebdbe0ab741a7ad155b942a4375`.
First-party (same owner), vendored rather than path-depended so a clean clone of
aterm builds the fabric bridge and the shipped binary carries it. Before this,
`crates/aterm-link` path-depended on a sibling checkout most clones do not have,
which is why no release ever shipped the bridge.

## What is here, and what is not

| crate | why |
|---|---|
| `astream-wire` | the vocabulary: Frame codec, Subject/Filter, offsets. Zero deps |
| `astream-cap` | the capability mint. Needs `sha2`; adds its seven-package tree to native builds |
| `astream-broker` | the bus itself, `cap` only. Its `asb` bin is NOT vendored |
| `astream-aead` | the sealed TCP transport. Present but NOT BUILT by default |

`astream-aead` is vendored so the sealed cross-host transport is one feature flag
away (`-p aterm-link --features sealed`), not so it ships. A default build must
not grow chacha20poly1305, getrandom and the dalek curves for a transport a local
fleet never uses — this repository counts third-party packages, and
`crates/aterm-digest` exists because `sha2` + `hmac` cost eight of them.

Not vendored: `astream-agent`, `astream-engine`, `astream-effects`,
`astream-evidence`, `astream-host`, `astream-kafka`, `astream-live`,
`astream-pump`, `astream-term`. Nothing on aterm's side references them.

## Review record

The 18 retained Rust files were compared byte for byte with that upstream commit
on 2026-09-10; they are unchanged. The four manifests are adapted as listed below.
`UPSTREAM.toml` pins the reviewed file inventory and SHA-256 digests, including the
adapted manifests; it records a review, not an authenticated upstream signature.
The upstream commit has no root LICENSE file. Its workspace and these manifests
declare Apache-2.0; the accompanying LICENSE is the complete Apache-2.0 text from
aterm's root, supplied here rather than claimed to be a retained upstream file.

`forge attest` checks the recorded files against the tracked checkout and Cargo
metadata, including the actual direct dependency paths. This is a named direct
source bundle, not a crates.io patch. The existing forge ownership metric still
counts its code under vendor/ as third-party: 11,122 lines in broker/cap/wire,
plus 48,869 in sha2 and its six dependencies. The bridge therefore adds ten
third-party packages, 59,991 lines, and one build script to each native graph.
Being present in Cargo.lock beforehand did not make those dependencies free.
Both browser graphs and the default sealed-transport exclusion are unchanged.

## Re-syncing

```sh
for c in astream-wire astream-cap astream-aead astream-broker; do
  rsync -a --delete --exclude target ../astream/crates/$c/ vendor/astream/crates/$c/
done
rm -rf vendor/astream/crates/astream-broker/src/bin          # `asb` is not vendored
rm -rf vendor/astream/crates/*/tests vendor/astream/crates/*/benches
```

**`--delete` is load-bearing.** Without it rsync only ADDS, so a re-sync silently
restores every file this tree deliberately drops — `src/bin/asb.rs` among them,
which brings back the three lint failures that are the reason it is not here. The
two `rm -rf` lines then remove what upstream has and this tree does not; they are
listed rather than folded into an `--exclude` so a reader can see exactly what is
missing and why.

Then re-apply the manifest edits this tree carries. None touches the crates'
SOURCE, and the list is exhaustive on purpose — an omitted one is how a re-sync
produces a tree that neither builds nor matches its pins.

1. Workspace-inherited `[package]` fields become explicit, at these EXACT values:

   ```toml
   version = "0.1.0"        # NOT upstream's 0.0.0 — see below
   edition = "2021"
   license = "Apache-2.0"
   authors = ["Andrew Yates"]
   rust-version = "1.89"
   ```

   **`version` deliberately diverges from upstream, and must.**
   `crates/aterm-forge/src/direct_vendor.rs` hard-requires `0.1.0` in two places
   — the manifest check (`"{name} manifest has unreviewed version"`) and the
   cargo-metadata cross-check that the package resolves to this tree and not to a
   registry. A syncer who faithfully copied upstream's `0.0.0` would fail
   `aterm forge attest`, which is why the value is written out here rather than
   described as "whatever upstream says".

   `rust-version` must be upstream's real floor: the broker calls
   `File::try_lock` (stable 1.89), and a lower number fails
   `clippy::incompatible_msrv` under this repo's `-D warnings`.
2. `{ workspace = true }` dependencies become paths, and `sha2` a version.
3. `[dev-dependencies]` is dropped — aterm does not run astream's suites, and the
   broker's dev-dependency on `astream-agent` is not vendored.
4. The broker's feature table keeps every upstream feature NAME (the source
   references them in `cfg(feature = …)`, and a missing one is an
   `unexpected_cfg` warning, which is an error under this repo's `-D warnings`),
   with only the comments rewritten to say what aterm does and does not enable.

## Re-syncing to a NEW upstream commit

Everything above keeps the SAME revision. Moving to a new one additionally
requires editing Rust, in three places the recipe cannot reach:

| file | what | why |
|---|---|---|
| `crates/aterm-forge/src/direct_vendor.rs` | `REVISION` | attest refuses an "unreviewed upstream revision" |
| `crates/aterm-forge/src/direct_vendor.rs` | `RECORD_SHA256` | the digest OF `UPSTREAM.toml`: "the source inventory changed without a renewed review" |
| `crates/aterm-forge/src/provenance.rs` | the `upstream:` string | attest prints it verbatim in its OB-1 line |

`RECORD_SHA256` fires FIRST and swallows the per-file diagnostics, so refresh it
last, after `UPSTREAM.toml` is final. Regenerate the per-file SHA-256 pins in
`UPSTREAM.toml` in the same pass — they are what makes a silent drift detectable,
and they are what the review those constants stand for actually reviewed. `src/bin/asb.rs` and its `[[bin]]` section are dropped too: `aterm link broker`
and `aterm link mint` are the two things aterm needs from that CLI, they are
first-party code in `crates/aterm-link/src/cli.rs`, and carrying upstream's bin
meant carrying its lint surface — this repository builds with `-D warnings` and
astream's own floor is looser, so `asb.rs` failed three lints that are not
astream's bug and not aterm's to fix in a vendored copy.

After a re-sync, review the upstream revision and every source/manifest delta,
then refresh UPSTREAM.toml and the coupled forge baseline/budget with the measured
reason. An unreviewed file, byte change, or dependency-path change fails attest.
