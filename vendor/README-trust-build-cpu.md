# Trust native CPU leases

`trust-build-cpu` is original Andrew Yates code shared with the Trust compiler,
not an alternate implementation of its lease protocol. This directory packages
Trust's `crates/trust-build-cpu` from commit
`2011393128c0e697f57c45ab8e8b034a2c55fac3` (version 0.12.0).

The Rust source files and upstream README are byte-identical to that commit.
The manifest materializes the upstream workspace's version, edition, licence,
author name and dependency requirements, and declares a separate workspace.
Upstream licence texts are retained. No runtime implementation was changed.

The local path makes both the private hermetic snapshot and the anonymous public
clone complete: neither needs an unpublished Git commit or a private repository.
The release's registry/path-only source rule remains enforced. The account-level
pool path, descriptor lifetime and protocol remain Trust's, so the compiler and
aterm continue observing the same leases.

Update this component from one explicit Trust commit, including all source
files, README and licence texts together. Record the new pin and exact source
hashes here; a local implementation change belongs in Trust first, followed by a
new import. Refresh the aterm lockfile and run the library's tests and its
aterm-capacity consumers before selecting it for a release.

Current upstream file SHA-256 values:

- `README.md`: `61e9661addc85c9fc0d226a3e52ed12bd4abc265ecf01f5b1b6931dceed047bf`
- `src/lib.rs`: `3ae180c8dd53e7ab58ed25d818c222e3d26e5486fdadd616238bf489bbbcc3d6`
- `src/unix.rs`: `fffe0df56b3bd729f28bdd4cf7ca8bc0639b2d73373733b3f7a43aa09a4d3203`
- `src/tests.rs`: `8225ad98b23afb4ef5ca7d42fdf4c6f178ee16fd9deddd23cfe60f1f518a28e3`
- `src/retained.rs`: `16b04d74183340e3dd91b9b6f25ff6410906efb8b95325588cd01c4056149fdc`
- `src/retained_tests.rs`: `183153ac3410aaf222fe449e1087977846e3221498d02fd4e2b238b2e5c4d95f`
- `LICENSE-MIT`: `b71bd43a069ca0641a9ecfe585ca7b3c53b5cc1608f8b68321168698e28b5ea1`
- `LICENSE-APACHE`: `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a`

This increment preserves the default account v1 pool and adds an explicitly
fresh private retained domain. It syncs debt before returning a non-cloneable
controller; Drop and owner death retain debt. Only original capability custody
and caller-established conclusive work closure permit settlement. This import
does not activate a global v2 budget, post-crash recovery or host GNU issuance.
