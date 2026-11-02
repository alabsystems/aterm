<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 AND MIT -->

# Mail cryptography provenance

Proton SRP client flow and published test vectors are adapted from Proton AG's
`proton-srp`, MIT, at proton-crypto-rs commit
`c468e282625206eda9b48d539949e6183b1da32a`. The MIT license is retained here.
https://github.com/ProtonMail/proton-crypto-rs

Only client authentication is retained. SHA-512 and Ed25519 verification use
aterm's existing ring dependency. Encoding uses aterm-codec, random bytes use
aterm-uds, and fixed-width modular arithmetic uses aterm-mail-bigint. No upstream
SRP, PGP, random, hashing or error-macro crate is linked.

The bcrypt core is adapted from bcrypt 0.17.1 (MIT), and its Blowfish key
expansion and constants from RustCrypto blowfish 0.9.1 (MIT OR Apache-2.0).
The license texts are retained. Only bcrypt's password hashing operation is
retained; there is no random-salt generator or public block-cipher interface.

`aes_fixslice.rs` is adapted from RustCrypto `aes` 0.8.4's 64-bit fixsliced
implementation (MIT OR Apache-2.0), originally by Alexandre Adomnicai. Its
framework-specific array types were replaced with native arrays. The cipher
framework is not linked. `aes.rs` supplies aterm's bounded CFB and key-wrap
operations and published known-answer checks.

The OpenPGP packet reader and writer are aterm code implementing v4 Curve25519
ECDH, integrity-protected encrypted messages and secret-key unlocking. Their
interoperability fixtures come from Proton AG's `proton-rpgp` at the commit
above; fixture provenance is recorded in `../../tests/fixtures/README.md`.
The upstream OpenPGP library is not linked or imported wholesale.
