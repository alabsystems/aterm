# vendor/astream

The astream crates aterm's fabric bridge (crates/aterm-link) builds against —
`astream-wire`, `astream-cap`, `astream-broker` and `astream-aead` — as plain
files from astream commit `d72c846c2426d20b3b1868fdef8240df9718dd21`: each crate's `Cargo.toml` and `src/`,
and astream's root `Cargo.toml`, whose workspace fields they inherit. Apache-2.0
(`LICENSE`). The root `Cargo.toml` excludes this directory from aterm's
workspace, so the crates build as dependencies and nothing else.
