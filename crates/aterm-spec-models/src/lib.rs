// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TLA+ specs of aterm's isolation boundary (sandbox, path confinement,
//! fork/exec, write-all, alt screen, GPU encode) in `specs/*.tla` + `.cfg`,
//! model-checked by `ty` from `tests/model_check.rs`. Bounded state machines
//! elsewhere are `ty_model!` models in `aterm-spec`.

use std::path::{Path, PathBuf};

/// The directory holding the `.tla`/`.cfg` specs, for consumers (the spec
/// cross-reference gate) that parse them as `SpecModule::External` anchor targets.
pub fn specs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("specs")
}
