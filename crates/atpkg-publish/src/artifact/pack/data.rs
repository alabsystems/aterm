// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Install explicitly listed data files from the checkout's committed tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::error::{Error, Result};

pub(super) fn declaration(recipe: Option<&aterm_json::Value>) -> Result<Option<String>> {
    recipe
        .and_then(|recipe| recipe.get("data-files"))
        .map(|value| {
            value
                .as_str()
                .filter(|path| safe_path(path))
                .map(str::to_owned)
                .ok_or_else(|| {
                    Error::refused("atpkg data-files must name a checkout-relative manifest")
                })
        })
        .transpose()
}

fn safe_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !part.is_empty()
            && !part.starts_with('.')
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
}

fn blob(checkout: &Path, revision: &str, path: &str) -> Result<Vec<u8>> {
    let entry = crate::git::run(
        checkout,
        &["--no-replace-objects", "ls-tree", revision, "--", path],
    )?;
    let Some((object, name)) = entry.split_once('\t') else {
        return Err(Error::refused(format!(
            "missing atpkg data file {path} at {revision}"
        )));
    };
    let Some(oid) = object.strip_prefix("100644 blob ").filter(|_| name == path) else {
        return Err(Error::refused(format!(
            "atpkg data file {path} must be a committed regular non-executable file"
        )));
    };
    crate::git::run_bytes(checkout, &["--no-replace-objects", "cat-file", "blob", oid])
}

pub(super) fn stage(program: &str, checkout: &Path, manifest: &str, root: &Path) -> Result<()> {
    let revision = crate::git::head(checkout)?;
    let list = blob(checkout, &revision, manifest)?;
    let list = std::str::from_utf8(&list)
        .map_err(|_| Error::refused("atpkg data manifest must be UTF-8"))?;
    let Some((target, files)) = list.split_once('\n') else {
        return Err(Error::refused(
            "atpkg data manifest requires a destination and files",
        ));
    };
    let share = format!("share/{program}");
    if !safe_path(target) || (target != share && !target.starts_with(&format!("{share}/"))) {
        return Err(Error::refused(format!(
            "atpkg data destination must be under {share}"
        )));
    }
    let source = manifest.rsplit_once('/').map(|(dir, _)| dir);
    let mut selected = BTreeSet::new();
    let mut path_spellings = BTreeMap::new();
    for path in files.split_terminator('\n') {
        if !safe_path(path) || !selected.insert(path) {
            return Err(Error::refused(format!(
                "invalid or duplicate atpkg data path {path:?}"
            )));
        }
        for prefix in path
            .match_indices('/')
            .map(|(end, _)| &path[..end])
            .chain([path])
        {
            if let Some(previous) = path_spellings.insert(prefix.to_ascii_lowercase(), prefix)
                && previous != prefix
            {
                return Err(Error::refused(format!(
                    "conflicting atpkg data paths {previous:?} and {prefix:?}"
                )));
            }
        }
    }
    if selected.is_empty() {
        return Err(Error::refused("atpkg data manifest has no files"));
    }
    for path in selected {
        let source = source.map_or_else(|| path.to_owned(), |dir| format!("{dir}/{path}"));
        let bytes = blob(checkout, &revision, &source)?;
        let dest = root.join(target).join(path);
        let parent = dest.parent().expect("data path has a package parent");
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::io(format!("mkdir {}", parent.display()), &err))?;
        std::fs::write(&dest, bytes)
            .map_err(|err| Error::io(format!("stage {}", dest.display()), &err))?;
    }
    Ok(())
}
