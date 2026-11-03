// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `aterm-census mirror audit`: the registry held to Clean's own record of what it proved.
//!
//! The census reads aterm's tree alone, so nothing in `verify census` sees a row Clean proves
//! that the registry lacks, or a harness that copies an item the registry does not watch. The
//! audit reads Clean's VF5 F1 fixture directory (the one holding `rows.out.tsv` and `blobs/`)
//! and names:
//! - each aterm row `rows.out.tsv` grades proved that no registry row records, and each
//!   registry row it does not grade proved;
//! - each registry row whose harness is missing, or whose items differ from what the harness's
//!   `// aterm:` header says it copied;
//! - each harness with an `// aterm:` header that no registry row names.

use std::collections::BTreeSet;
use std::path::Path;

use super::Row;

/// The items a harness's `// aterm:` header names, as `file#path` (a `*` glob kept as
/// written), or `None` for a harness with no header.
///
/// The header is the comment from its `// aterm:` line to the first comment line that is
/// empty or a doc comment. Its first sentence lists the items: a root-relative file, then the
/// items of that file in backticks, comma-separated, each optionally `the body of` one;
/// parenthesized asides and segments of other words ("the `use` of its variants", "REWRITTEN
/// for F1") name nothing. A qualified item (`WaitEnd::echo`) whose type the header named
/// earlier lives in that type's file, wherever the list stands.
#[must_use]
pub fn header_items(harness: &str) -> Option<Vec<String>> {
    let mut text = String::new();
    let mut on = false;
    for line in harness.lines() {
        let t = line.trim_start();
        if !on {
            if let Some(rest) = t.strip_prefix("// aterm:") {
                on = true;
                text.push_str(rest);
            }
            continue;
        }
        match t.strip_prefix("//") {
            Some(rest) if !rest.starts_with('/') && !rest.trim().is_empty() => {
                text.push(' ');
                text.push_str(rest.trim());
            }
            _ => break,
        }
    }
    if !on {
        return None;
    }
    let mut flat = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let mut segments = Vec::new();
    let mut code = false;
    let mut start = 0;
    let b = flat.as_bytes();
    let mut end = flat.len();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'`' => code = !code,
            b',' if !code => {
                segments.push(&flat[start..i]);
                start = i + 1;
            }
            b'.' if !code && b.get(i + 1).is_none_or(u8::is_ascii_whitespace) => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    segments.push(&flat[start..end.max(start)]);
    let mut file = String::new();
    let mut items = Vec::new();
    let mut homes: Vec<(String, String)> = Vec::new();
    for segment in segments {
        let mut s = segment.trim();
        if s.starts_with("crates/") || s.starts_with("apps/") {
            let (path, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
            file = path.to_string();
            s = rest.trim();
        }
        let s = s.strip_prefix("the body of ").unwrap_or(s);
        if let Some(name) = s.strip_prefix('`').and_then(|n| n.strip_suffix('`'))
            && !name.contains('`')
            && !file.is_empty()
        {
            let owner = name
                .trim_start_matches('<')
                .split([':', ' '])
                .next()
                .unwrap_or("");
            let home = name
                .contains("::")
                .then(|| homes.iter().find(|(n, _)| n == owner))
                .flatten()
                .map_or(file.as_str(), |(_, f)| f.as_str());
            items.push(format!("{home}#{name}"));
            homes.push((name.to_string(), file.clone()));
        }
    }
    Some(items)
}

/// Does the header's `want` (`file#path`, maybe ending in `*`) cover `item`?
fn covers(want: &str, item: &str) -> bool {
    want.strip_suffix('*')
        .map_or(want == item, |prefix| item.starts_with(prefix))
}

/// What `vf5` (Clean's VF5 F1 fixture directory) says that `rows` do not, and how many rows
/// had items to compare (a harness with no `// aterm:` header lists none: its row's items are
/// as recorded, unchecked).
#[must_use]
pub fn audit(rows: &[Row], vf5: &Path) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut compared = 0;
    let graded = match crate::tree::read_to_string(&vf5.join("rows.out.tsv")) {
        Ok(text) => proved_rows(&text),
        Err(e) => {
            return (
                vec![format!("{}: {e}", vf5.join("rows.out.tsv").display())],
                0,
            );
        }
    };
    let recorded: BTreeSet<&str> = rows.iter().map(|r| r.row.as_str()).collect();
    for row in graded.difference(&recorded.iter().map(|r| (*r).to_string()).collect()) {
        out.push(format!(
            "Clean grades aterm row `{row}` proved, and the registry records no row `{row}`"
        ));
    }
    let blobs = vf5.join("blobs");
    let mut named = BTreeSet::new();
    for row in rows {
        if !graded.contains(&row.row) {
            out.push(format!(
                "registry row `{}` is not an aterm row rows.out.tsv grades proved",
                row.row
            ));
        }
        named.insert(row.harness.clone());
        let Ok(harness) = crate::tree::read_to_string(&blobs.join(&row.harness)) else {
            out.push(format!(
                "registry row `{}`: no harness blobs/{}",
                row.row, row.harness
            ));
            continue;
        };
        let Some(header) = header_items(&harness) else {
            continue;
        };
        compared += 1;
        let items: Vec<String> = row.items.iter().map(super::Item::render).collect();
        for want in &header {
            if !items.iter().any(|i| covers(want, i)) {
                out.push(format!(
                    "registry row `{}` does not watch `{want}`, which blobs/{} copies",
                    row.row, row.harness
                ));
            }
        }
        for item in &items {
            if !header.iter().any(|w| covers(w, item)) {
                out.push(format!(
                    "registry row `{}` watches `{item}`, which blobs/{}'s header does not name",
                    row.row, row.harness
                ));
            }
        }
    }
    for rel in harnesses(&blobs, "") {
        if !named.contains(&rel)
            && crate::tree::read_to_string(&blobs.join(&rel))
                .ok()
                .and_then(|h| header_items(&h))
                .is_some()
        {
            out.push(format!(
                "harness blobs/{rel} copies aterm code, and no registry row names it"
            ));
        }
    }
    (out, compared)
}

/// The aterm rows `rows.out.tsv` grades proved: a row (second field) graded `proved…` whose
/// source names an aterm file (` src/`) or whose blob is the R208 mirror (`aterm`).
fn proved_rows(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let proved = f.iter().any(|x| x.starts_with("proved"));
            let aterm = f.first() == Some(&"aterm") || f.iter().any(|x| x.contains(" src/"));
            (proved && aterm && f.len() > 1).then(|| f[1].to_string())
        })
        .collect()
}

/// Every `.rs` under `dir`, relative to the blobs root.
fn harnesses(root: &Path, rel: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let sub = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            out.extend(harnesses(root, &sub));
        } else if name.ends_with(".rs") {
            out.push(sub);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_names_its_items() {
        let harness = "#![allow(dead_code)]\n// D-2(b) rows.\n\n\
            // aterm: crates/aterm-log/src/lib.rs `MAX`, `Level` (derives dropped: a `Clone`\n\
            // reads through a pointer), the `use` of its variants, crates/x/src/a.rs\n\
            // `<A as From<bool>>::from`, the body of `g` (rewritten), `Level::f`. The `x`.\n\
            //\n// `ignored`\n";
        assert_eq!(
            header_items(harness),
            Some(vec![
                "crates/aterm-log/src/lib.rs#MAX".to_string(),
                "crates/aterm-log/src/lib.rs#Level".to_string(),
                "crates/x/src/a.rs#<A as From<bool>>::from".to_string(),
                "crates/x/src/a.rs#g".to_string(),
                "crates/aterm-log/src/lib.rs#Level::f".to_string(),
            ])
        );
        assert_eq!(header_items("// no header\n"), None);
        assert!(covers("f#MASK_*", "f#MASK_A") && !covers("f#MASK_*", "g#MASK_A"));
    }

    #[test]
    fn proved_rows_are_aterm_rows_graded_proved() {
        let text = "# blob\trow\n\
                    bv_x\tx\t0\tsym\tproved (calculus)\t{}\taterm-x src/lib.rs f\n\
                    aterm\ty\t1\tsym\tproved (calculus)\n\
                    wrong_x\tx_wrong\tunsafe\t`x` panics\n\
                    collision\tc\t0\tsym\tproved (calculus)\n";
        assert_eq!(
            proved_rows(text),
            BTreeSet::from(["x".to_string(), "y".to_string()])
        );
    }
}
