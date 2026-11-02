<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: MIT -->

These are public test fixtures from Proton AG's proton-rpgp, at
proton-crypto-rs commit c468e282625206eda9b48d539949e6183b1da32a:

- `test-data/keys/private_key_v4.asc`
- `test-data/messages/encrypted_message_v4.asc`
- `test-data/signatures/signature_v4.asc`

The private key is a published test key, not an account credential. The expected
decrypted message is `hello world`. The original MIT license is retained at
`../../src/crypto/LICENSE-Proton-MIT`.

The key armor has one added `Comment:` header identifying this public fixture to
the secret scanner. Its encoded key payload is unchanged from the pinned upstream
file.
