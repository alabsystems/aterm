# Vendored shared native resource provenance

Upstream: Trust `1f4f39cd0f69a0e86c201698637913c2ad65956d`, `crates/trust-resource-provenance`.

All Rust source, README and license bytes are exact upstream. The manifest
only redirects libc/serde/JSON/hash/tempfile to maintained first-party local
crates; no dependency bypass or wire/parser fork. The upstream lock is kept
as provenance; the enclosing workspace resolves its maintained dependencies.

This is the read-only authority-discovery implementation shared with Trust.
Host creation/cache publication and actual admission/custody stay with aterm.

Upstream SHA-256:

- `6839a6f9079207fa7a8033df175b52491f8b265b67688a85006f924af360eece` `Cargo.lock`
- `2d4a402d69c308d6863e83ebbbf00fe667b9e9f7891346e4e475b055b737ab9a` `Cargo.toml`
- `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a` `LICENSE-APACHE`
- `b71bd43a069ca0641a9ecfe585ca7b3c53b5cc1608f8b68321168698e28b5ea1` `LICENSE-MIT`
- `ff24b8753e76ee0b50769cc17f66cc237a6fc2ca27e9b708f088899486084190` `README.md`
- `a68fd86809a713c2f859ae5afa707d633bfafb05e29bf38639c3085b215cfbdb` `src/connection.rs`
- `0621ad0a2e3328d789d96d6a1647910c95c7ef7e62fda372f5a2878d9a5513c9` `src/lib.rs`
- `0ce09536938125506543bf7dfaaf556f55b69758bcc6730c225b736e126b9be7` `src/private.rs`
- `d38c91ad3c7530eef8554a578568362ca629995aedf0d2cbeb3d66123734c2ce` `src/private/tests.rs`
- `d0cf4c1827f85e18c6484e3f620ca64b4dd50f842f92883ef81b4f0dacd2e5a1` `src/process.rs`
- `fa6cc08dd8279e61e9bfa290a0b7298fcdb8b55460610f16e252aa995498272e` `src/process/tests.rs`
- `9acaddeb0a2ff9d9fe3e3b1041aa5112030632edd2d003d8d6dfd5589284332f` `src/reader.rs`
- `3950bd296333bfa3cd59be90bc2ca17c4f71d2cfbcfac5143e67e4745895f086` `src/reader/tests.rs`
- `0fa0d2a25fdc272f4928401a261cca6f8e01edb2ab53e10546a12f5d18f22238` `src/schema.rs`
- `5c42754466a403f60e72c8396f6b3d8da97d61fe376c7e1dd8392e63439d5b83` `src/schema/tests.rs`
