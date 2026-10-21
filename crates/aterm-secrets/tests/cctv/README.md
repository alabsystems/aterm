<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# C2SP CCTV age vectors (the X25519-only subset)

These files are test vectors from the C2SP Community Cryptography Test Vectors project,
<https://github.com/C2SP/CCTV>, directory `age/testdata`, at commit `50a8ecf2a220`
(2026-09-25). They are © 2022 The age Authors and available under the
[Zero-Clause BSD](https://opensource.org/licenses/0BSD),
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/) or
[Unlicense](https://unlicense.org/) licence, at the user's choice; no attribution is
required, and this note gives it anyway.

`tests/cctv.rs` decrypts every one with `aterm_secrets::age::decrypt` and holds it to its
exact class: `success` to the SHA-256 of the plaintext, every failure to the one
`AgeError` it names (`no match` → `NoMatch`, `HMAC failure` → `Hmac`, `header failure` →
`Header`, `payload failure` → `Payload`, `armor failure` → `Armor`).

## What was imported, and how

Of CCTV's 147 age vectors, the 102 that need neither a passphrase (`scrypt`) nor a hybrid
post-quantum identity (`mlkem768x25519`): every vector whose identities are all X25519, plus
the two that carry no identity at all (`empty`, `armor_empty`). Each CCTV file is a textual
header, an empty line and the age file; the import split them:

- `<name>.age`: the age file, byte for byte.
- `<name>.age.zlib`: the age file still zlib-compressed, as CCTV ships its 20 large vectors
  (three of them are 16 MiB uncompressed); the test inflates them with aterm-codec.
- `index.tsv`: one row per vector, tab-separated: name, CCTV's `expect`, the `payload`
  SHA-256 in hex (or `-`), the identities as the lowercase hex of their 32 secret bytes,
  comma-separated (or `-`), and whether CCTV marks the file armored (`y`/`n`).

The identities are stored as hex rather than as `AGE-SECRET-KEY-1…` strings so that a secret
scanner reading this repository does not flag published test keys. CCTV's `file key` and
`comment` fields were dropped.

For a `payload failure`, CCTV also gives the hash of whatever a streaming reader released
before the failure; `decrypt` is all-or-nothing and releases nothing then, so only the
`success` hashes are checked.
