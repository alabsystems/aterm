# vendor/astream

The astream crates aterm's fabric bridge (crates/aterm-link) builds against —
`astream-aead`, `astream-broker`, `astream-cap`, `astream-digest`, `astream-wire` — as plain
files from astream commit `6aab5d26a40560b686f6a5f93e4189ce7567eb9b`: each crate's `Cargo.toml` and `src/`,
and astream's root `Cargo.toml`, whose workspace fields they inherit. Apache-2.0
(`LICENSE`). The root `Cargo.toml` excludes this directory from aterm's
workspace, so the crates build as dependencies and nothing else.

That commit is on astream's development line. https://github.com/alabsystems/astream
carries astream's own releases, each published as a snapshot commit of its own, so
that commit's id does not appear there, and the version these manifests declare
may not be released there yet.
