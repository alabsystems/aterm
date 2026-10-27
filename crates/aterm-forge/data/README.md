<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Offline Brew help capture

`abrew-help.json` is the input to `targo --unverified forge abrew-help`. It is
independent of `crates/atpkg/src/abrew/help_texts.rs`; a missing or broken generated
file does not prevent the tool from rebuilding it. The command checks by default.
`--write` publishes only after input validation, Trust formatting and sealing
succeed. Trust's formatter is the only child program; Homebrew and Python are
never invoked.

The initial snapshot migrates the already-sealed Homebrew 7.0.10 capture from
aterm commit `f15a698638e026b4ecc927b555dcdf985632f072`. This is a change of input
format, with no new reference measurement. The explicit bootstrap is:

```text
targo --unverified forge abrew-help import-legacy --input /path/to/sealed-f15.rs --output crates/aterm-forge/data/abrew-help.json
```

The importer accepts that exact artifact's SHA-256 and seal, parses only its
TOP/TEXTS/ALIASES declarations, and refuses an existing snapshot. Normal generation
never falls back to importing the generated output.

For a future reference version, supply an independently captured JSON snapshot
inside the workspace, then pass its relative path with `--input`. Commit the
snapshot alongside the regenerated Rust. The schema has `schema: 1`, a recorded
`reference` beginning with `Homebrew `, `mode: "piped"`, `columns`, a nonempty honest
`provenance`, the source capture's `source_sha256`, an optional `source_commit`, the
exact `top` string, `canonical` records containing `name` and `stdout`, and `aliases` records containing
`name` and `target`. An alias can also carry `stdout`, which must equal the named
canonical record. Aliases always name their target; text equality never chooses
one implicitly. Duplicate names, collisions, alias chains and unknown targets are
errors. Strings retain trailing whitespace, line endings and Unicode.

Snapshot paths use portable ASCII components so the generated header records a
portable rerun command. Captures must not contain a person's home path; documented
defaults such as `/opt/homebrew` and `/home/linuxbrew/.linuxbrew` remain valid.
