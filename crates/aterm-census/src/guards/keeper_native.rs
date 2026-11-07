// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native GUI evidence in the library/build sources of each resolved process dependency.

use std::collections::{BTreeSet, VecDeque};

use super::rules::{KEEPER_GUI_DEPS, links_gui_dependency};
use super::{Finding, LoadKind, SourceFile, Tree};

pub(super) fn inspect(tree: &Tree<'_>, dir: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let finding = |path: String, detail: String| Finding {
        rule: KEEPER_GUI_DEPS.id,
        path,
        line: 0,
        detail,
    };
    let roots = match crate::scan_set::dependency_source_roots(tree.root, dir) {
        Ok(roots) => roots,
        Err(why) => {
            out.push(finding(format!("{dir}/Cargo.toml"), why));
            return out;
        }
    };
    let mut queue: VecDeque<_> = roots.into_iter().map(|path| (path, true)).collect();
    let mut seen = BTreeSet::new();
    let mut scanned = BTreeSet::new();
    while let Some((rel, dir_owner)) = queue.pop_front() {
        if !seen.insert((rel.clone(), dir_owner)) {
            continue;
        }
        let owned;
        let file = if let Some(file) = tree.corpus.get(&rel) {
            file
        } else {
            match crate::tree::read_to_string(&tree.root.join(&rel)) {
                Ok(text) => {
                    owned = SourceFile::new(rel.clone(), text);
                    &owned
                }
                Err(why) => {
                    out.push(finding(rel, format!("unreadable process source: {why}")));
                    continue;
                }
            }
        };
        if scanned.insert(rel.clone()) {
            for line in file.shipped_lines() {
                if let Some(what) = links_gui_dependency(line.text) {
                    out.push(file.finding(
                        KEEPER_GUI_DEPS.id,
                        line.no,
                        format!("{what}, inside the keeper's process closure"),
                    ));
                }
            }
        }
        for load in file
            .loads_in(dir_owner, Some(dir))
            .iter()
            .filter(|load| !load.gated)
        {
            let mut found = false;
            for rel in &load.files {
                if tree.corpus.get(rel).is_none() && !crate::tree::is_file(&tree.root.join(rel)) {
                    continue;
                }
                found = true;
                let owner = match load.kind {
                    LoadKind::Mod => rel.ends_with("/mod.rs"),
                    LoadKind::PathMod => true,
                    LoadKind::Include => dir_owner,
                };
                queue.push_back((rel.clone(), owner));
            }
            if !found {
                out.push(finding(
                    rel.clone(),
                    format!("unreadable process module: {}", load.files.join(" or ")),
                ));
            }
        }
    }
    out
}
