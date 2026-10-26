# vendor/astream

The astream crates aterm builds against (the fabric bridge, crates/aterm-link,
and the conformance suite's differential VT oracle) —
`astream-aead`, `astream-broker`, `astream-cap`, `astream-digest`, `astream-effects`, `astream-sys`, `astream-term`, `astream-wire` — as plain
files from astream commit `da3424bfb3dc8bc2457e6bc9b6d601414667dc9f`: each crate's tree but its `tests/`,
`examples/` and `benches/`, and astream's root `Cargo.toml`, whose workspace
fields they inherit, with its member list emptied (it names crates not copied
here). Apache-2.0 (`LICENSE`).
aterm's root `Cargo.toml` excludes this directory from aterm's workspace, so the
crates build as dependencies and nothing else.

That commit is on astream's development line. https://github.com/alabsystems/astream
carries astream's own releases, each published as a snapshot commit of its own, so
that commit's id does not appear there, and the version these manifests declare
may not be released there yet.
