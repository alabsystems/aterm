# Versioning

aterm uses a three-component version, `MAJOR.MINOR.DEV`:

- **`DEV = 0` is a public release version** (`0.1.0`, `0.2.0`, `1.0.0`). This is
  the only form a published snapshot ever carries.
- **`DEV > 0` marks internal development iterations** (`0.1.1`, `0.1.2`, …). The
  workspace version in `Cargo.toml` sits here between public releases.

After a public `X.Y.0` ships, internal work bumps the DEV counter. The next
public release bumps `MINOR` (or `MAJOR`) and resets `DEV` to `0`.

## Release

The public-source publication (`publish/transforms.sh`, driven by the
publication engine) normalizes the workspace and every first-party crate from
the internal `X.Y.DEV` down to the public `X.Y.0`, then tags the public snapshot
`v X.Y.0`. First-party crates are detected structurally (a `[[package]]` with no
`source` line in `Cargo.lock`), so the normalization is exact regardless of
which versions third-party crates happen to use.

This matches the constellation-wide scheme used across the ALab repositories.
