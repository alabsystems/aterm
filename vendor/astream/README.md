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
  rsync -a --exclude target ../astream/crates/$c/ vendor/astream/crates/$c/
done
```

Then re-apply the three manifest edits this tree carries, none of which touch
the crates' source: workspace-inherited `[package]` fields become explicit,
`{ workspace = true }` dependencies become paths (and `sha2` a version), and
`[dev-dependencies]`/`[[example]]`/`[[bench]]` are dropped — aterm does not run
astream's suites, and the broker's dev-dependency on `astream-agent` is not
vendored. `src/bin/asb.rs` and its `[[bin]]` section are dropped too: `aterm link broker`
and `aterm link mint` are the two things aterm needs from that CLI, they are
first-party code in `crates/aterm-link/src/cli.rs`, and carrying upstream's bin
meant carrying its lint surface — this repository builds with `-D warnings` and
astream's own floor is looser, so `asb.rs` failed three lints that are not
astream's bug and not aterm's to fix in a vendored copy.

After a re-sync, review the upstream revision and every source/manifest delta,
then refresh UPSTREAM.toml and the coupled forge baseline/budget with the measured
reason. An unreviewed file, byte change, or dependency-path change fails attest.
