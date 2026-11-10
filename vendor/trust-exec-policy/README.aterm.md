# Canonical native exec policy

Rust sources, the upstream README and lockfile are copied exactly from Trust
`cae6f4b58466174749dde6f0d7d0bbf1fcc5e052`, `crates/trust-exec-policy`.
The source pin is verified against the actual Git tree before publication;
the full commit identifier is recorded by this adoption's decision receipt.
The MIT and Apache licenses come from the same canonical tree. The standalone
manifest adds only the explicit `package.metadata.aterm-vendor` origin record.

The App root redirects libc to its maintained generated declarations. The
opaque Apple attribute handle avoids requiring a new generated ABI declaration.
No admission, installed-policy receipt, sandbox profile supplied by a request,
or managed solver capability is created by copying this source.
