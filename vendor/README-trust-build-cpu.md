# Trust native CPU leases

`trust-build-cpu` is original Andrew Yates code shared with the Trust compiler,
not an alternate implementation of its lease protocol. This directory packages
Trust's `crates/trust-build-cpu` from commit
`afda6b4602be9bebc79e3ec20ad2aba40449bd7f` (version 0.12.0).

The three Rust files and upstream README are byte-identical to that commit.
The manifest materializes the upstream workspace's version, edition, licence,
author name and dependency requirements, and declares a separate workspace.
Upstream licence texts are retained. No runtime implementation was changed.

The local path makes both the private hermetic snapshot and the anonymous public
clone complete: neither needs an unpublished Git commit or a private repository.
The release's registry/path-only source rule remains enforced. The account-level
pool path, descriptor lifetime and protocol remain Trust's, so the compiler and
aterm continue observing the same leases.

Update this component from one explicit Trust commit, including all three source
files, README and licence texts together. Record the new pin and exact source
hashes here; a local implementation change belongs in Trust first, followed by a
new import. Refresh the aterm lockfile and run the library's tests and its
aterm-capacity consumers before selecting it for a release.

Current upstream file SHA-256 values:

- `README.md`: `0c9eb30adae00c0122cdfc9158f919e8cea796ce7f831b9a344268c365b955d1`
- `src/lib.rs`: `178618103d006b79fbc3a221854626505465f957f2f874ad6ef0bce4bd02bc08`
- `src/unix.rs`: `05761702c3d440b0b56f8a95f547dfbce065e4e24cd4c03897eb6e768732b50b`
- `src/tests.rs`: `00843aed61d644cb5d550d0250fbeb2aead57b01dc901608b16542167a89ae3a`
