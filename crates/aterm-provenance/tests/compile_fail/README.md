# Compile-fail fixtures

These ui/*.rs files encode negative guarantees from the provenance
framework design — things that must **not** compile:

| Fixture                                         | Negative guarantee |
|-------------------------------------------------|--------------------|
| `forge_host_from_pty.rs`                        | `Provenance<_, Pty>` cannot coerce to `Provenance<_, Host>` without `authorize_pty_to_host` |
| `forge_user_from_pty.rs`                        | `Provenance<_, Pty>` cannot coerce to `Provenance<_, User>` (there is no such ceremony) |
| `forge_host_auth_token_without_feature.rs`      | `HostAuthorizationToken::__new_for_capability_only` is gated behind `aterm-provenance/internal-mint`; a caller without the feature hits E0599 (#8013) |

Phase 0 ships the fixtures as source files only. Wiring them into a
`trybuild` harness is a follow-up (adds the `trybuild` dev-dep, which
the zero-external-dependency campaign audits at the workspace level).

The `forge_host_auth_token_without_feature.rs` fixture restates the
feature gate `aterm-core`'s `capability_ceremony` test checks lexically.

See the provenance framework design's Phase 0 acceptance criteria.
