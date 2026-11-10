// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A new independent package cannot remove an old workspace member merely by
//! being excluded. This is a contextual fact, not a TOML-only exception.

use super::{Members, Replan, git, manifest_tables_replan, parse_toml};
use std::{collections::BTreeSet, ffi::OsStr, path::Path};

pub(super) fn replan(
    root: &Path,
    path_env: &OsStr,
    base_commit: &str,
    base: Option<&str>,
    work: Option<&str>,
    members: &Members,
) -> Option<Replan> {
    let (base, mut work) = (parse_toml(base)?, parse_toml(work)?);
    let before = base.get("workspace")?.as_table()?;
    let after = work.get("workspace")?.as_table()?;
    for field in ["members", "default-members"] {
        if before.get(field) != after.get(field) {
            return None;
        }
    }
    let old = exclusions(before.get("exclude"))?;
    let new = exclusions(after.get("exclude"))?;
    if !old.is_subset(&new) || old == new || members.is_empty() {
        return None;
    }
    // Strip only this candidate difference. Resolver, profile, package and
    // dependency changes still pass through the ordinary manifest classifier.
    let mut table = after.clone();
    match before.get("exclude") {
        Some(value) => {
            table.insert("exclude".into(), value.clone());
        }
        None => {
            table.remove("exclude");
        }
    }
    work.insert("workspace".into(), aterm_toml::Value::Table(table));
    let mut seeds = match manifest_tables_replan(&base, &work) {
        Replan::Nothing => BTreeSet::new(),
        Replan::Packages(packages) => packages,
        Replan::Everything(_) => return None,
    };
    let tree = git(root, path_env)
        .args([
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            "--name-only",
            base_commit,
        ])
        .output()
        .ok()?;
    if !tree.status.success() {
        return None;
    }
    let entries: Vec<&str> = tree
        .stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(std::str::from_utf8)
        .collect::<Result<_, _>>()
        .ok()?;
    for added in new.difference(&old) {
        if !literal_path(added) || entries.iter().any(|entry| overlaps_base(added, entry)) {
            return None;
        }
        if members.0.iter().any(|member| overlaps(added, &member.dir)) {
            return None;
        }
        // Unknown vendor code must still widen and receive Forge's provenance
        // verdict. A directory name is never a general vendor exemption.
        if (added == "vendor" || added.starts_with("vendor/"))
            && !aterm_forge::provenance::roster_row(added)
                .is_some_and(|row| row.dir == added.as_str())
        {
            return None;
        }
        let mut directory = root.to_path_buf();
        for component in added.split('/') {
            directory.push(component);
            let metadata = std::fs::symlink_metadata(&directory).ok()?;
            if !metadata.file_type().is_dir() {
                return None;
            }
        }
        let text = crate::read_regular_text(directory.join("Cargo.toml")).ok()?;
        let document = parse_toml(Some(&text))?;
        let workspace = document.get("workspace")?.as_table()?;
        if !exclusions(workspace.get("members"))?.is_empty() {
            return None;
        }
        let package = document.get("package")?.as_table()?;
        if package.get("workspace").is_some() {
            return None;
        }
        let name = package.get("name")?.as_str()?;
        let version = package.get("version")?.as_str()?;
        if name.is_empty() || version.is_empty() {
            return None;
        }
        // Force the ordinary real graph/inversion to account for every added
        // package, including same-name, off-host and unconsumed packages. A
        // failed or empty inversion retains select()'s existing widening.
        seeds.insert(format!("{name}@{version}"));
    }
    Some(Replan::Packages(seeds))
}

fn exclusions(value: Option<&aterm_toml::Value>) -> Option<BTreeSet<String>> {
    let Some(value) = value else {
        return Some(BTreeSet::new());
    };
    let array = value.as_array()?;
    let set: BTreeSet<String> = array
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    (set.len() == array.len()).then_some(set)
}

fn literal_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        })
}

fn overlaps(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.strip_prefix(b).is_some_and(|tail| tail.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|tail| tail.starts_with('/'))
}

fn overlaps_base(added: &str, entry: &str) -> bool {
    // Git names are case-sensitive; a checkout need not be. An unknown Unicode
    // component under the shared prefix cannot prove absence on every host.
    for (wanted, existing) in added.split('/').zip(entry.split('/')) {
        if !existing.is_ascii() {
            return true;
        }
        if !wanted.eq_ignore_ascii_case(existing) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
