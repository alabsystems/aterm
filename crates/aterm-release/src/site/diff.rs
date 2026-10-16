// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A dry run's view of a planned page: a unified line diff, three lines of context.

/// The lines of `new` that differ from `old`, as `-`/`+`/` ` rows in hunks headed
/// `@@ -a,n +b,m @@`, under a `--- path` / `+++ path (planned)` header.
#[must_use]
pub fn unified(path: &str, old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // the longest common subsequence, by suffix lengths
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    // the edit script: (kind, old index, new index)
    let mut script = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            script.push((' ', i, j));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            script.push(('-', i, j));
            i += 1;
        } else {
            script.push(('+', i, j));
            j += 1;
        }
    }
    const CONTEXT: usize = 3;
    let changed: Vec<usize> = (0..script.len()).filter(|k| script[*k].0 != ' ').collect();
    let mut out = Vec::new();
    if changed.is_empty() {
        return out;
    }
    out.push(format!("--- {path}"));
    out.push(format!("+++ {path} (planned)"));
    let mut k = 0;
    while k < changed.len() {
        let first = changed[k].saturating_sub(CONTEXT);
        let mut last = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= last + 2 * CONTEXT + 1 {
            k += 1;
            last = changed[k];
        }
        let last = (last + CONTEXT).min(script.len() - 1);
        let rows = &script[first..=last];
        let old_n = rows.iter().filter(|r| r.0 != '+').count();
        let new_n = rows.iter().filter(|r| r.0 != '-').count();
        out.push(format!(
            "@@ -{},{old_n} +{},{new_n} @@",
            rows[0].1 + 1,
            rows[0].2 + 1
        ));
        for (kind, i, j) in rows {
            let line = if *kind == '+' { b[*j] } else { a[*i] };
            out.push(format!("{kind}{line}"));
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_changed_lines_and_their_context_are_shown() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
        assert_eq!(
            unified("p", old, new),
            [
                "--- p",
                "+++ p (planned)",
                "@@ -2,8 +2,9 @@",
                " b",
                " c",
                " d",
                "-e",
                "+E",
                " f",
                " g",
                " h",
                " i",
                "+j",
            ]
        );
        assert!(unified("p", old, old).is_empty());
    }
}
