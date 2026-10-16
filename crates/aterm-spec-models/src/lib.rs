// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLA+ specs of aterm's isolation boundary (sandbox, path confinement,
//! fork/exec, write-all, alt screen, GPU encode) in `specs/*.tla` + `.cfg`,
//! model-checked by `ty` from `spec-check specs` (`aterm-spec`), and the `ay`
//! certificate bundles under `proofs/ay`, re-checked by `spec-check ay`.
//! Bounded state machines elsewhere are `ty_model!` models in `aterm-spec`.
