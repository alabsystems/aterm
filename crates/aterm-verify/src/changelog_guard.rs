// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-set structural changelog judgment, over the changed file on the merged checkout.

use crate::{Ctx, Scope};
use aterm_census::changelog::{CHANGELOG_FILE, HARNESS_ENTRY_HEAD, findings};

/// Nothing to judge when this change does not touch the changelog. Listing failures remain
/// failures to run, rather than silently omitting the check.
pub fn selected(ctx: &Ctx) -> Result<bool, String> {
    // The release owns its section selection: a recut's fresh Unreleased is legitimately
    // empty, and its primary changelog gate judges the rolled version through the same rules.
    if ctx.release {
        return Ok(false);
    }
    let paths = match &ctx.input_paths {
        Some(paths) => paths.clone(),
        None => {
            let base = match &ctx.scope {
                Scope::Changed(changed) => changed.base.as_str(),
                _ => "origin/main",
            };
            crate::changed::changed_since(&ctx.root, &ctx.path_env, base)?
        }
    };
    Ok(paths.iter().any(|path| path == CHANGELOG_FILE))
}

/// Judge the working tree, including staged/unstaged changes. The land driver merges and
/// rehomes before constructing this selection, so this is the file it will push.
pub fn judge(ctx: &Ctx) -> Result<Option<Vec<String>>, String> {
    if !selected(ctx)? {
        return Ok(None);
    }
    let text = std::fs::read_to_string(ctx.root.join(CHANGELOG_FILE))
        .map_err(|e| format!("read {CHANGELOG_FILE}: {e}"))?;
    let mut found = findings(&text, "Unreleased", None);
    if aterm_census::changelog::real_body(&text, "Unreleased")
        .iter()
        .any(|line| line.starts_with(HARNESS_ENTRY_HEAD))
    {
        match crate::harness_debt::baseline(
            &ctx.root,
            ctx.head_at_start.as_deref().unwrap_or("HEAD"),
            &ctx.path_env,
        ) {
            Ok((version, _)) => found = findings(&text, "Unreleased", Some(&version)),
            Err(why) if found.is_empty() => return Err(why),
            Err(why) => found.push(format!("harness window could not be checked: {why}")),
        }
    }
    Ok(Some(found))
}

#[cfg(test)]
#[path = "changelog_guard_tests.rs"]
mod tests;
