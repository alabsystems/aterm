# Maintained native resource sources

Library Rust sources are copied from canonical Trust 6441b55c27ed2da4cd6bda5f85ce40277d22ef70,
`crates/trust-resource-controller`. Local manifests redirect maintained dependencies;
standalone binary and example targets are outside this library snapshot.
The canonical source has authenticated API and native Git readback. Current
Aterm qualification is tracked independently.

The Bootstrap source-offer files and four-description channel extension are
copied exactly from canonical Trust `44d395c5530760f3390f3861551e1d0a53fdf280`.
The matching client SourceBound phase and object client are also copied from
that source. Its changed-source native graph tests passed before this import. The base
library, local dependency redirection and existing member overlay are retained.
The channel feature enables the existing optional Trust SHA256 dependency for
bounded positional record checks; the DTO-only leaf still disables defaults.

The contained member mode retains the actual original native target retirement,
GNU descriptions and accepted Host transport. Protocol data, marker declarations
and copied source create no execution, SDK, input release, CPU or proof authority.
Paired and runtime-input Host route activation is outside this contribution.

The existing Aterm Main `src/client/member.rs` lifecycle deadline repair is
retained as an explicit overlay: its original transport observation is bounded
by 60 seconds, rather than reverting to the older upstream 8-second interval.
This file is outside the contained-mode shared source equality scope.
Its existing test-only `fixture_deadline` support is retained in `src/lib.rs`.
