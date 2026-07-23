# Compile-fail fixtures

These ui/*.rs files encode negative guarantees from the provenance
framework design — things that must **not** compile:

| Fixture                                         | Negative guarantee |
|-------------------------------------------------|--------------------|
| `forge_host_from_pty.rs`                        | `Provenance<_, Pty>` cannot coerce to `Provenance<_, Host>` without `authorize_pty_to_host` |
| `forge_user_from_pty.rs`                        | `Provenance<_, Pty>` cannot coerce to `Provenance<_, User>` (there is no such ceremony) |
| `top_is_not_origin.rs`                          | `Top` does not implement `Origin`; `Provenance<T, Top>` is uninstantiable |
| `erase_without_lint.rs`                         | `Provenance::into_inner_erased` is `#[deprecated]`; use under `-D deprecated` fails |
| `forge_host_auth_token_without_feature.rs`      | `HostAuthorizationToken::__new_for_capability_only` is gated behind `aterm-provenance/internal-mint`; a caller without the feature hits E0599 (#8013) |
| `forge_network_auth_token_without_feature.rs`   | `NetworkAuthorizationToken::__new_for_capability_only` — same gate as `forge_host_auth_token_without_feature` (#8013) |

Phase 0 ships the fixtures as source files only. Wiring them into a
`trybuild` harness is a follow-up (adds the `trybuild` dev-dep, which
the zero-external-dependency campaign audits at the workspace level).

The `erase_without_lint.rs` fixture is additionally enforced by the provenance
erasure checker, which greps for `into_inner_erased` call
sites and fails if any live outside `security::` modules without a
`// PROVENANCE-ERASE: <reason>` audit comment on the same or previous line.

The `forge_{host,network}_auth_token_without_feature.rs` fixtures are
additionally enforced by the provenance ceremony checker, which
verifies (a) the constructors remain feature-gated, (b) only allow-listed
crates enable the feature, and (c) only allow-listed crates call the
constructors. Run via `make check-provenance-ceremony` or the composite
`make check-seals` target.

See the provenance framework design's Phase 0 acceptance criteria.
