// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Preserve the local packer's failure before checking its output protocol.

use super::{ChildOut, Error, GitRun, Redactor, Result, failed, kept};

pub(super) fn hash<'a>(
    packed: &'a ChildOut,
    run: &GitRun,
    redactor: &Redactor,
    index: usize,
    objects: usize,
) -> Result<&'a str> {
    if packed.code != Some(0) {
        let error = failed(&run.op, packed, redactor, &run.slug, false);
        let why = format!(
            "checkout blob batch {index} ({objects} objects), git pack-objects: {}",
            error.why
        );
        let kind = error.kind;
        return Err(error.recast(kind, why));
    }
    let truncated = packed.stdout_may_be_truncated();
    if !truncated
        && let Some(hash) = std::str::from_utf8(&packed.stdout)
            .ok()
            .and_then(|value| value.strip_suffix('\n'))
            .filter(|value| crate::path::is_sha(value))
    {
        return Ok(hash);
    }
    let reason = if truncated {
        "truncated stdout"
    } else {
        "an invalid pack hash"
    };
    Err(Error::protocol(format!(
        "checkout blob batch {index} ({objects} objects), git pack-objects returned {reason}; stdout_bytes={}; stdout: {}; stderr: {}",
        packed.stdout.len(),
        kept(&packed.stdout, redactor),
        kept(&packed.stderr, redactor),
    ))
    .with_repo(run.slug.to_string()))
}

#[cfg(test)]
#[path = "checkout_pack_tests.rs"]
mod tests;
