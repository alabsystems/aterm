# Trust native CPU leases

`trust-build-cpu` is original Andrew Yates code shared with the Trust compiler,
not an alternate implementation of its lease protocol. This directory packages
Trust's `crates/trust-build-cpu` from commit
`4cfdfb2c23be00fcd895c78756de77f3b7f0c04d` (version 0.12.0).

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
- `src/unix.rs`: `fc8b3232bdf27c34b6c3a43b7c8fb95e694e1c070d648394d1ddbc302fc776c3`
- `src/tests.rs`: `712af9b254584dce445b4e9ceefa19190da2becd9ec3cc050c35cf2538f2466b`
